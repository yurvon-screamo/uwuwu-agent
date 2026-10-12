import { Plugin } from "@opencode/plugin"
import { appendFileSync } from "node:fs"

// goal-mode — режим цели для обычных сессий OpenCode V2.
//
// Постановка: команда /goal <цель> (шаблон в opencode.json кладёт в чат
// сообщение "goal: <цель>"). Плагин регистрирует цель на сессию; когда
// сессия уходит в idle (прогон завершён целиком), запускается СУДЬЯ — временная
// сессия на отдельной модели в нейтральной директории, которая проверяет
// достижение цели ПО ФАКТАМ (файлы, коммиты, выводы — не по пересказу агента).
//
// Вердикты судьи (первый токен ответа):
//   GOAL_DONE        — цель достигнута: цель снимается, в чат летит подтверждение
//   GOAL_NOT_DONE    — в чат летит критика + synthetic-пинок агенту «продолжай»
//   GOAL_IMPOSSIBLE  — цель недостижима: снимается с объяснением
//
// Стоп-краны: команда /goal-stop снимает цель руками; судейский
// GOAL_IMPOSSIBLE закрывает недостижимые цели. Счётчиков итераций нет —
// единственный арбитр прогресса судья и пользователь.

const LOG_PATH = `${process.env.HOME ?? "."}/.local/share/opencode/log/goal-mode.log`
const MARKER = /^\s*goal:\s*/i
const STOP = /^\s*goal-stop\b/i
const JUDGE_MODEL = { providerID: "zai-coding-plan", modelID: "glm-5.3-flash" }
const JUDGE_DIR_FALLBACK = "/tmp"
const JUDGE_TIMEOUT_MS = 10 * 60_000
const TRANSCRIPT_CAP = 60_000
const RESUBSCRIBE_MS = 5_000

const log = (message: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${message}\n`) } catch {}
}

interface Goal { text: string; since: number }
interface JudgeWait {
  resolve: (text: string) => void
  segments: Map<string, string> // key: assistantMessageID:ordinal -> текст сегмента
}

const goals = new Map<string, Goal>()          // sessionID -> активная цель
const judging = new Set<string>()              // сессии, где судья сейчас работает
const judgeWait = new Map<string, JudgeWait>() // sessionID судьи -> ожидание вердикта

// Финал судьи = последнее assistant-сообщение его сессии (тот же принцип,
// что в хабе: ранние сегменты — «думает вслух» между тулами)
const lastMessageText = (segments: Map<string, string>): string => {
  let lastMsg = ""
  for (const key of segments.keys()) if (key.includes(":")) lastMsg = key.split(":")[0]
  if (!lastMsg) return [...segments.values()].join("\n").trim()
  return [...segments.entries()]
    .filter(([key]) => key.startsWith(`${lastMsg}:`))
    .sort((a, b) => Number(a[0].split(":")[1]) - Number(b[0].split(":")[1]))
    .map(([, text]) => text)
    .join("\n")
    .trim()
}

const judgePrompt = (goal: string, transcript: string): string =>
  `Ты — строгий судья достижения цели. Проверь цель по ФАКТАМ: файлы, коммиты, выводы команд (используй инструменты чтения). Пересказу агента не верь — проверяй. Слабые или косвенные улики считай недостаточными.

ЦЕЛЬ: ${goal}

ПЕРЕПИСКА С МОМЕНТА ПОСТАНОВКИ ЦЕЛИ (USER — пользователь, AGENT — агент):
${transcript || "(пусто)"}

Ответь СТРОГО одной строкой, первый токен — вердикт:
GOAL_DONE: <чем подтверждается достижение>
GOAL_NOT_DONE: <конкретно что не сделано>
GOAL_IMPOSSIBLE: <почему цель недостижимо>`

export default Plugin.define({
  id: "goal-mode",
  setup: async (ctx: any) => {
    const runJudge = async (sessionID: string, goal: Goal): Promise<void> => {
      // директория проекта целевой сессии — судья должен видеть её файлы
      let dir = JUDGE_DIR_FALLBACK
      try {
        const s: any = await ctx.session.get({ sessionID })
        const projects: any[] = await ctx.project.list()
        const p = projects.find((x: any) => x.id === s.projectID)
        if (p?.canonical) dir = p.canonical
      } catch (e) { log(`dir lookup: ${e}`) }

      let transcript = ""
      try {
        const r: any = await ctx.session.message({ sessionID })
        const msgs: any[] = r?.data ?? r ?? []
        transcript = msgs
          .filter((m: any) => (m.time?.created ?? 0) >= goal.since && (m.type === "user" || m.type === "assistant"))
          .map((m: any) => {
            const who = m.type === "user" ? "USER" : "AGENT"
            const t = (m.content ?? [])
              .filter((p: any) => p?.type === "text" && p?.text)
              .map((p: any) => p.text)
              .join("\n")
              .trim()
            return t ? `${who}: ${t}` : ""
          })
          .filter(Boolean)
          .join("\n\n")
          .slice(-TRANSCRIPT_CAP)
      } catch (e) { log(`transcript ${sessionID}: ${e}`) }

      const js: any = await ctx.session.create({
        title: "goal-judge",
        model: JUDGE_MODEL,
        location: { directory: dir },
      } as never)

      const verdict = await new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => {
          if (judgeWait.delete(js.id)) reject(new Error("judge timeout"))
        }, JUDGE_TIMEOUT_MS)
        judgeWait.set(js.id, {
          segments: new Map(),
          resolve: (t) => { clearTimeout(timer); resolve(t) },
        })
        void ctx.session
          .prompt({ sessionID: js.id, text: judgePrompt(goal.text, transcript) } as never)
          .catch((e: unknown) => {
            clearTimeout(timer)
            if (judgeWait.delete(js.id)) reject(e)
          })
      })

      try { await ctx.session.remove({ sessionID: js.id } as never) } catch {}

      const m = verdict.match(/^\s*(GOAL_DONE|GOAL_NOT_DONE|GOAL_IMPOSSIBLE)\s*:?\s*(.*)/i)
      const detail = (m?.[2] ?? verdict).trim().slice(0, 600)
      const kind = m?.[1]?.toUpperCase()

      if (kind === "GOAL_DONE") {
        goals.delete(sessionID)
        log(`DONE ${sessionID}: ${detail}`)
        await ctx.session.synthetic({ sessionID, text: `[goal-mode] ✅ судья подтвердил достижение цели: ${detail}` } as never)
        return
      }
      if (kind === "GOAL_IMPOSSIBLE") {
        goals.delete(sessionID)
        log(`IMPOSSIBLE ${sessionID}: ${detail}`)
        await ctx.session.synthetic({ sessionID, text: `[goal-mode] ⛔ судья: цель недостижима — ${detail}. Цель снята.` } as never)
        return
      }
      log(`NOT_DONE ${sessionID}: ${detail.slice(0, 100)}`)
      await ctx.session.synthetic({
        sessionID,
        text: `⚖️ GOAL-судья: цель «${goal.text}» ещё НЕ достигнута. ${detail}\nПродолжай работу над целью до полного выполнения. Не отчитывайся словами — доноси до результата.`,
      } as never)
    }

    const subscription = new AbortController()
    void (async () => {
      // стрим событий может рваться — переподписка навсегда, пока плагин жив
      while (!subscription.signal.aborted) {
        try {
          for await (const event of ctx.event.subscribe({ signal: subscription.signal }) as AsyncIterable<any>) {
            const type: string = event?.type ?? ""
            const d: any = event?.data ?? {}
            if (!type) continue

            // 1) регистрация цели: user-сообщение с маркером "goal: ..."
            if (type === "session.inbox.enqueued") {
              const item = d.item ?? {}
              const sid: string | undefined = d.sessionID
              if (!sid || item?.type !== "user" || typeof item?.payload?.text !== "string") continue
              const text = item.payload.text
              if (STOP.test(text)) {
                if (goals.delete(sid)) log(`goal-stop ${sid}: цель снята пользователем`)
                continue
              }
              const goalText = text.replace(MARKER, "").trim()
              if (goalText !== "" && goalText !== text) {
                goals.set(sid, { text: goalText, since: Date.now() })
                log(`goal set ${sid}: ${goalText.slice(0, 80)}`)
              }
              continue
            }

            // 2) сбор текста судьи (его сессия ждёт вердикта)
            if (type === "session.text.delta" || type === "session.text.ended") {
              const sid: string | undefined = d.sessionID
              const wait = sid ? judgeWait.get(sid) : undefined
              if (!wait || typeof (d.delta ?? d.text) !== "string") continue
              const key = `${d.assistantMessageID}:${d.ordinal}`
              const seg = wait.segments.get(key) ?? ""
              wait.segments.set(key, type === "session.text.ended" ? d.text : seg + d.delta)
              continue
            }

            // 3) судья закончил — отдаём вердикт ожидающему
            if (type === "session.execution.succeeded") {
              const wait = judgeWait.get(d.sessionID)
              if (!wait) continue
              judgeWait.delete(d.sessionID)
              wait.resolve(lastMessageText(wait.segments))
              continue
            }

            // 4) сессия ушла в idle — прогон завершён целиком: время суда
            if (type === "session.idle") {
              const sid: string = d.sessionID
              const goal = goals.get(sid)
              if (!goal || judging.has(sid)) continue
              judging.add(sid)
              log(`judging ${sid}: ${goal.text.slice(0, 80)}`)
              void runJudge(ctx, sid, goal)
                .catch((e: unknown) => log(`judge ${sid}: ${e}`))
                .finally(() => judging.delete(sid))
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
