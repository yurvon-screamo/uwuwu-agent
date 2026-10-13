import { Plugin } from "@opencode/plugin"
import { appendFileSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { homedir } from "node:os"
import {
  parseSchedule,
  parseCronFields,
  nextFire,
  renderBrief,
  describeSchedule,
  compactResult,
  fmtLocal,
  peakHoursWarning,
  validateTaskInput,
  newId,
  type CronTask,
  type Schedule,
} from "../lib/cron-lib"

// cron-mode — планировщик задач сессий. Интерфейс — типизированные тула:
//   cron_set    — поставить (одноразово: in/at; постоянно: every/cron)
//   cron_list   — живые задачи текущей сессии
//   cron_remove — снять по id
//
// При срабатывании в сессию летит БРИФ — самодостаточный промпт с историей
// прошлого прогона (переживает компакшн). Доставка — session.prompt (queue:
// агент занят — промпт ждёт). Пропущенный за простой триггер догоняется раз.
// session.deleted → задачи сессии умирают.
//
// Задачи хранятся в ctx.storage (переживает рестарты). Инстансов плагина
// несколько (по локации): тикер у каждого, дубль гасится lastFiredAt в
// хранилище — чей-то свежий fire виден всем при перечитывании перед fire.

const LOG_PATH = join(homedir(), ".local/share/opencode/log/cron-mode.log")
const TASKS_KEY = "cron-mode:tasks"
const TICK_MS = 30_000
const RESULT_CAP = 500
const MIN_TASK_MS = 60_000
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

const log = (message: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${message}\n`) } catch {}
}

// ——— REST: последний assistant-текст сессии (для lastResult) ———

interface RestMessage {
  type: string
  content?: Array<{ type?: string; text?: string }>
}

async function lastAssistantText(sessionID: string): Promise<string> {
  try {
    const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
    const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
    const auth = "Basic " + Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")
    const res = await fetch(`${base}/api/session/${sessionID}/message`, {
      headers: { authorization: auth },
    })
    const j: unknown = await res.json()
    const msgs: unknown = (j as { data?: unknown }).data ?? j
    if (!Array.isArray(msgs)) return ""
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i] as RestMessage
      if (m.type !== "assistant") continue
      const t = (m.content ?? [])
        .filter((p) => p?.type === "text" && p?.text)
        .map((p) => p.text ?? "")
        .join("\n")
        .trim()
      if (t !== "") return t
    }
  } catch (e) { log(`rest: ${e}`) }
  return ""
}

// ——— хранилище задач (ctx.storage) ———

interface Storage {
  read: () => Promise<CronTask[]>
  update: (fn: (tasks: CronTask[]) => CronTask[]) => Promise<void>
}

function createStorage(ctx: Parameters<Parameters<typeof Plugin.define>[0]["setup"]>[0]): Storage {
  const read = async (): Promise<CronTask[]> => {
    try {
      const stored = await ctx.storage.get(TASKS_KEY)
      const arr: unknown = (stored as { tasks?: unknown } | undefined)?.tasks
      if (!Array.isArray(arr)) return []
      const out: CronTask[] = []
      for (const t of arr as CronTask[]) {
        if (
          t && typeof t.id === "string" && typeof t.sessionID === "string" &&
          typeof t.prompt === "string" && t.schedule &&
          typeof (t.schedule as Schedule).kind === "string"
        ) out.push(t)
      }
      return out
    } catch (e) { log(`storage read: ${e}`); return [] }
  }
  const update = async (fn: (tasks: CronTask[]) => CronTask[]): Promise<void> => {
    try {
      const snapshot = JSON.parse(JSON.stringify(fn(await read())))
      await ctx.storage.set(TASKS_KEY, { tasks: snapshot })
    } catch (e) { log(`storage write: ${e}`) }
  }
  return { read, update }
}

// ——— плагин ———

export default Plugin.define({
  id: "cron-mode",
  setup: async (ctx) => {
    const storage = createStorage(ctx)
    const firing = new Set<string>()

    const fire = async (task: CronTask, note = ""): Promise<void> => {
      if (firing.has(task.id)) return
      firing.add(task.id)
      try {
        // Перечитать свежее состояние: параллельный инстанс мог уже фаернуть
        const latest = (await storage.read()).find((t) => t.id === task.id)
        if (latest === undefined) return // задачу сняли
        const halfMs = latest.schedule.kind === "every" ? latest.schedule.intervalMs / 2 : 30_000
        if (latest.lastFiredAt !== undefined && Date.now() - latest.lastFiredAt < halfMs) return
        // РЕЗЕРВ ДО fire: инстанс, прочитавший резерв, скипнет (анти-дубль)
        const prev = latest.lastFiredAt
        const reservedAt = Date.now()
        latest.lastFiredAt = reservedAt
        await storage.update((all) => all.map((t) => (t.id === latest.id ? { ...t, lastFiredAt: reservedAt } : t)))
        try {
          latest.lastResult = compactResult(await lastAssistantText(latest.sessionID), RESULT_CAP)
          const brief = renderBrief(latest, Date.now()) + (note ? `\n(${note})` : "")
          await ctx.session.prompt({ sessionID: latest.sessionID, text: brief })
          latest.fireCount++
          if (latest.schedule.kind === "once") {
            // одноразовая — снята после исполнения
            await storage.update((all) => all.filter((t) => t.id !== latest.id))
          } else {
            await storage.update((all) => all.map((t) => (t.id === latest.id ? { ...latest } : t)))
          }
          log(`fire #${latest.id} → ${latest.sessionID}`)
        } catch (e) {
          // откат резерва: следующий тик честно ретрайнет
          await storage.update((all) => all.map((t) => (t.id === latest.id ? { ...t, lastFiredAt: prev } : t)))
          log(`fire #${latest.id}: ${e} — резерв откачен`)
        }
      } finally {
        firing.delete(task.id)
      }
    }

    const tick = async (): Promise<void> => {
      const tasks = await storage.read()
      const now = Date.now()
      const due = tasks.filter((task) => {
        const halfMs = task.schedule.kind === "every" ? task.schedule.intervalMs / 2 : 30_000
        if (task.lastFiredAt !== undefined && now - task.lastFiredAt < halfMs) return false // чужой свежий fire
        const next = nextFire(task.schedule, task.lastFiredAt ?? task.createdAt)
        return next !== null && now >= next
      })
      for (const task of due) await fire(task)
    }

    // тикер + catch-up: у каждого инстанса, дубль гасится lastFiredAt
    void tick().catch((e: unknown) => log(`tick: ${e}`))
    const timer = setInterval(() => {
      void tick().catch((e: unknown) => log(`tick: ${e}`))
    }, TICK_MS)

    // сессия удалена — её задачи умирают вместе с ней
    const eventsAbort = new AbortController()
    void (async () => {
      while (!eventsAbort.signal.aborted) {
        try {
          for await (const event of ctx.event.subscribe({ signal: eventsAbort.signal })) {
            if (event.type !== "session.deleted") continue
            const sid = event.data.sessionID
            const before = (await storage.read()).length
            await storage.update((all) => all.filter((t) => t.sessionID !== sid))
            const after = (await storage.read()).length
            if (after !== before) log(`session ${sid} удалена — снято задач: ${before - after}`)
          }
        } catch (e) {
          log(`event stream: ${e} — переподписка через 5с`)
        }
        if (eventsAbort.signal.aborted) return
        await new Promise((r) => setTimeout(r, 5_000))
      }
    })()

    // ——— тула (namespace+codemode — как в офиц. доках V2) ———

    await ctx.tool.transform((editor) => {
      editor.namespace({
        name: "cron",
        description: "Планировщик задач: одноразовые напоминания и периодические проверки сессии",
      })
      editor.add({
        name: "cron_set",
        description:
          "Поставить задачу по расписанию в ТЕКУЩУЮ сессию. РОВНО ОДИН параметр времени: in | at | every | cron. " +
          "prompt — САМОДОСТАТОЧНАЯ инструкция (что делать, что с результатом): при срабатывании исполняется " +
          "как отдельный бриф, без памяти этой беседы. Задача живёт, пока жива сессия. Время — локальное (Осака/JST).",
        input: {
          type: "object",
          properties: {
            name: { type: "string", description: "Короткое имя задачи" },
            prompt: { type: "string", description: "Самодостаточная инструкция: что делать, что с результатом, критерий снятия" },
            in: { type: "string", description: "Однократно через интервал: «20m», «90s», «2h»" },
            at: { type: "string", description: "Однократно в локальное время: «15:00»" },
            every: { type: "string", description: "Постоянно интервалом: «30m», «2h», «1d» (мин. 1m)" },
            cron: { type: "string", description: "Постоянно по cron (5 полей, локальное время): «0 9 * * 1-5»" },
            reportPolicy: { type: "string", description: "Что делать с результатом: «>80% — алерт, иначе тихо»" },
          },
          required: ["prompt"],
          additionalProperties: false,
        },
        options: { namespace: "cron", codemode: true },
        async execute(rawInput, toolCtx) {
          log(`cron_set input: ${JSON.stringify(rawInput)?.slice(0, 300)}`)
          const input = rawInput as { name?: string; prompt?: string; reportPolicy?: string } & Parameters<typeof parseSchedule>[0]
          const valid = validateTaskInput(input)
          if (!valid.ok) return { content: `❌ ${valid.error}` }
          const now = Date.now()
          const all = await storage.read()
          const schedule = parseSchedule(input, now)
          if (schedule === null) {
            const seen = { in: input.in, at: input.at, every: input.every, cron: input.cron }
            log(`cron_set input seen: ${JSON.stringify(seen)} / all: ${JSON.stringify(input).slice(0, 200)}`)
            return { content: `❌ укажи РОВНО ОДНО время: in | at | every | cron. Получено: ${JSON.stringify(seen)}` }
          }
          const id = newId((x) => all.some((t) => t.id === x))
          let prompt = valid.prompt
          if (input.reportPolicy) prompt += `\nПолитика отчёта: ${input.reportPolicy}`
          const task: CronTask = {
            id,
            name: input.name?.trim() || prompt.slice(0, 40),
            sessionID: toolCtx.sessionID,
            prompt,
            schedule,
            createdAt: now,
            createdBy: "агент",
            fireCount: 0,
          }
          await storage.update((all) => [...all, task])
          const next = nextFire(schedule, now)
          const warn = peakHoursWarning(next ?? now)
          log(`set #${id} ${toolCtx.sessionID}: ${describeSchedule(schedule)}`)
          return {
            content:
              `✓ задача #${id} «${task.name}» — ${describeSchedule(schedule)}; первый запуск ` +
              `${next === null ? "?" : fmtLocal(next)}${warn ? `; ${warn}` : ""}. ` +
              `Кратко сообщи пользователю, что задача поставлена и когда сработает.`,
          }
        },
      })
      editor.add({
        name: "cron_list",
        description: "Список задач по расписанию в текущей сессии: id, расписание, следующий запуск, последний результат.",
        input: { type: "object", properties: {}, additionalProperties: false },
        options: { namespace: "cron", codemode: true },
        async execute(_input, toolCtx) {
          const mine = (await storage.read()).filter((t) => t.sessionID === toolCtx.sessionID)
          if (mine.length === 0) return { content: "задач нет" }
          const lines = mine.map((t) => {
            const next = nextFire(t.schedule, t.lastFiredAt ?? t.createdAt)
            return `#${t.id} «${t.name}» · ${describeSchedule(t.schedule)} · след. ${next ? fmtLocal(next) : "?"}${t.lastResult ? ` · прошлое: ${t.lastResult.slice(0, 80)}` : ""}`
          })
          return { content: lines.join("\n") }
        },
      })
      editor.add({
        name: "cron_remove",
        description: "Снять задачу по id (см. cron_list).",
        input: {
          type: "object",
          properties: { id: { type: "string", description: "id задачи, напр. a3f2" } },
          required: ["id"],
          additionalProperties: false,
        },
        options: { namespace: "cron", codemode: true },
        async execute(rawInput) {
          const { id } = (rawInput ?? {}) as { id?: string }
          const clean = typeof id === "string" ? id.trim().replace(/^#/, "") : ""
          let removed = ""
          await storage.update((all) => {
            const task = all.find((t) => t.id === clean)
            if (task) removed = `✓ задача #${task.id} «${task.name}» снята`
            return all.filter((t) => t.id !== clean)
          })
          if (removed === "") return { content: "❌ задача не найдена (cron_list покажет живые)" }
          log(`remove #${clean}`)
          return { content: removed }
        },
      })
    })

    log(`cron-mode: on`)
    return () => {
      clearInterval(timer)
      eventsAbort.abort()
      log("cron-mode: off")
    }
  },
})
