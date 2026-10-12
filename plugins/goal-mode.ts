import { Plugin } from "@opencode/plugin"
import { appendFileSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { homedir } from "node:os"
import {
  parseGoalCommand,
  parseVerdict,
  renderTranscript,
  judgePrompt,
  lastMessageText,
  lastAssistantText,
  type Goal,
} from "./goal-lib"

// goal-mode — режим цели для обычных сессий OpenCode V2.
//
// Постановка: команда /goal <цель> (шаблон кладёт в чат "goal: <цель>").
// Плагин регистрирует цель на сессию; когда прогон завершается, запускается
// СУДЬЯ — временная сессия на отдельной модели в директории проекта, которая
// проверяет достижение цели ПО ФАКТАМ (файлы, коммиты — не по пересказу).
//
// Вердикты судьи (первый токен ответа):
//   GOAL_DONE        — цель достигнута: снимается, в чат летит подтверждение
//   GOAL_NOT_DONE    — в чат критика + synthetic-пинок агенту «продолжай»
//   GOAL_IMPOSSIBLE  — цель недостижима: снимается с объяснением
//
// Стоп-краны: /goal-stop снимает цель руками; судейский GOAL_IMPOSSIBLE
// закрывает недостижимые. Счётчиков итераций нет — арбитры судья и пользователь.
// Цели персистентны (ctx.storage) — перезагрузка плагина их не теряет.

const LOG_PATH = join(homedir(), ".local/share/opencode/log/goal-mode.log")
const GOAL_KEY = "goal-mode:goals"
const JUDGE_MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const
const JUDGE_TIMEOUT_MS = 10 * 60_000
const TRANSCRIPT_CAP = 60_000
const RESUBSCRIBE_MS = 5_000
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

const log = (message: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${message}\n`) } catch {}
}

interface JudgeWait {
  resolve: (text: string) => void
  segments: Map<string, string>
}

interface TranscriptMessage {
  type: string
  time?: { created?: number }
  content?: Array<{ type?: string; text?: string }>
}

// REST-доступ к серверу: в плагинном домене сессий нет чтения истории и удаления
let restBase: string | null = null
let restAuth: string | null = null

function restInit(): boolean {
  if (restBase !== null && restAuth !== null) return true
  try {
    const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
    restBase = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1") + "/api"
    restAuth = "Basic " + Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")
    return true
  } catch (e) { log(`service.json: ${e}`); return false }
}

async function restMessages(sessionID: string): Promise<TranscriptMessage[]> {
  if (!restInit()) return []
  const res = await fetch(`${restBase as string}/session/${sessionID}/message`, {
    headers: { authorization: restAuth as string },
  })
  const j: unknown = await res.json()
  const msgs: unknown = (j as { data?: unknown }).data ?? j
  return Array.isArray(msgs) ? (msgs as TranscriptMessage[]) : []
}

async function restRemoveSession(sessionID: string): Promise<void> {
  if (!restInit()) return
  await fetch(`${restBase as string}/session/${sessionID}`, {
    method: "DELETE",
    headers: { authorization: restAuth as string },
  }).catch(() => {})
}

export default Plugin.define({
  id: "goal-mode",
  setup: async (ctx) => {
    // Плагин инстанцируется на каждую локацию (5+ экземпляров), а события
    // видны всем — без гарда каждый экземпляр заводит своего судью на то же
    // событие (шторм пинков, проверено на живом). Подписывается ровно один.
    const GUARD = Symbol("goal-mode-subscribed")
    const globals = globalThis as { [GUARD]?: boolean }
    if (globals[GUARD]) {
      log("goal-mode: инстанс этой локации уже подписан — скип")
      return
    }
    globals[GUARD] = true

    // цели: память (быстро) + storage (переживает перезагрузки плагина)
    const goals = new Map<string, Goal>()
    try {
      const stored = await ctx.storage.get(GOAL_KEY)
      if (stored && typeof stored === "object") {
        for (const [sid, g] of Object.entries(stored as unknown as Record<string, Goal>)) {
          if (g !== null && typeof g === "object" && typeof g.text === "string" && typeof g.since === "number") {
            goals.set(sid, { text: g.text, since: g.since })
          }
        }
      }
      log(`restored ${goals.size} goals from storage`)
    } catch (e) { log(`storage restore: ${e}`) }
    const persist = async (): Promise<void> => {
      try { await ctx.storage.set(GOAL_KEY, JSON.parse(JSON.stringify(Object.fromEntries(goals)))) }
      catch (e) { log(`storage persist: ${e}`) }
    }

    const judging = new Set<string>()
    const judgeWait = new Map<string, JudgeWait>()

    const runJudge = async (sessionID: string, goal: Goal): Promise<void> => {
      let transcript = ""
      try {
        transcript = renderTranscript(await restMessages(sessionID), goal.since, TRANSCRIPT_CAP)
      } catch (e) { log(`transcript ${sessionID}: ${e}`) }

      const judge = await ctx.session.create({
        title: "goal-judge",
        model: { providerID: JUDGE_MODEL.providerID, id: JUDGE_MODEL.id },
        location: { directory: "/tmp" },
      })

      // ждём завершения судьи, затем читаем его вердикт из истории сессии
      await ctx.session.wait({ sessionID: judge.id })

      // флаш может запаздывать: одна повторная попытка через 3с
      let msgs = await restMessages(judge.id)
      if (lastAssistantText(msgs) === "") {
        await new Promise((r) => setTimeout(r, 3_000))
        msgs = await restMessages(judge.id)
      }
      await restRemoveSession(judge.id)

      const reply = lastAssistantText(msgs)

      const verdict = parseVerdict(reply)
      log(`verdict ${sessionID}: ${verdict.kind} — ${verdict.detail.slice(0, 100)}`)

      if (verdict.kind === "done") {
        goals.delete(sessionID)
        await persist()
        await ctx.session.synthetic({
          sessionID,
          text: `[goal-mode] ✅ судья подтвердил достижение цели: ${verdict.detail}`,
        })
        return
      }
      if (verdict.kind === "impossible") {
        goals.delete(sessionID)
        await persist()
        await ctx.session.synthetic({
          sessionID,
          text: `[goal-mode] ⛔ судья: цель недостижима — ${verdict.detail}. Цель снята.`,
        })
        return
      }
      await ctx.session.synthetic({
        sessionID,
        text: `⚖️ GOAL-судья: цель «${goal.text}» ещё НЕ достигнута. ${verdict.detail}\nПродолжай работу над целью до полного выполнения. Не отчитывайся словами — доноси до результата.`,
      })
    }

    const subscription = new AbortController()
    void (async () => {
      // стрим событий может рваться — переподписка навсегда, пока плагин жив
      while (!subscription.signal.aborted) {
        try {
          for await (const event of ctx.event.subscribe({ signal: subscription.signal })) {
            switch (event.type) {
              // 1) команды: /goal регистрирует цель, /goal-stop снимает
              case "session.inbox.enqueued": {
                const { sessionID, item } = event.data
                if (item.type !== "user") continue
                const parsed = parseGoalCommand(item.payload.text)
                if (parsed === null) continue
                if (parsed.kind === "stop") {
                  if (goals.delete(sessionID)) {
                    await persist()
                    log(`goal-stop ${sessionID}: цель снята пользователем`)
                  }
                  continue
                }
                goals.set(sessionID, { text: parsed.text, since: Date.now() })
                await persist()
                log(`goal set ${sessionID}: ${parsed.text.slice(0, 80)}`)
                continue
              }

              // 2) сбор текста судьи (его сессия ждёт вердикта)
              case "session.text.delta":
              case "session.text.ended": {
                const wait = judgeWait.get(event.data.sessionID)
                if (!wait) continue
                const key = `${event.data.assistantMessageID}:${event.data.ordinal}`
                const seg = wait.segments.get(key) ?? ""
                wait.segments.set(
                  key,
                  event.type === "session.text.ended"
                    ? (event.data.text ?? "")
                    : seg + (event.data.delta ?? ""),
                )
                continue
              }

              // 3) судья закончил — отдаём вердикт ожидающему
              case "session.execution.succeeded": {
                const sid = event.data.sessionID
                const wait = judgeWait.get(sid)
                if (wait) {
                  judgeWait.delete(sid)
                  wait.resolve(lastMessageText(wait.segments))
                  continue
                }

                // 4) прогон цели завершён — время суда. Пинок судьи запускает
                // следующий прогон, его успех снова триггерит судью —
                // цикл до DONE/IMPOSSIBLE/стопа.
                const goal = goals.get(sid)
                if (!goal || judging.has(sid)) continue
                judging.add(sid)
                log(`judging ${sid}: ${goal.text.slice(0, 80)}`)
                void runJudge(sid, goal)
                  .catch((e: unknown) => log(`judge ${sid}: ${e}`))
                  .finally(() => judging.delete(sid))
                continue
              }

              // 5) сессия удалена — цель больше не нужна
              case "session.deleted": {
                const sid = event.data.sessionID
                if (goals.delete(sid)) {
                  await persist()
                  log(`goal dropped: сессия ${sid} удалена`)
                }
                continue
              }

              default:
                continue
            }
          }
        } catch (e) {
          log(`event stream down: ${e} — переподписка через ${RESUBSCRIBE_MS / 1000}с`)
        }
        if (subscription.signal.aborted) return
        await new Promise((r) => setTimeout(r, RESUBSCRIBE_MS))
      }
    })()

    log("goal-mode: on")
    return () => {
      subscription.abort()
      log("goal-mode: off")
    }
  },
})
