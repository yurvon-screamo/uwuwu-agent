import { Plugin } from "@opencode/plugin"
import { appendFileSync, readFileSync, renameSync, writeFileSync, mkdirSync } from "node:fs"
import { join } from "node:path"
import { homedir } from "node:os"
import {
  parseSchedule,
  parseCronFields,
  nextFire,
  renderBrief,
  compactResult,
  newId,
  type CronTask,
  type Schedule,
} from "../lib/cron-lib"

// cron-mode — планировщик задач сессий. Интерфейс — файл
// ~/.config/opencode/cron-tasks.json (формат в skill schedule):
// агент дописывает задачу write-тулом, плагин исполняет по расписанию.
//
//   { "tasks": [ { "id": "a3f2", "name": "…", "sessionID": "ses_…",
//                  "prompt": "…", "schedule": { "kind": "every", "intervalMs": 7200000 } } ] }
//
// schedule: {kind:"once", at:epoch-ms} | {kind:"every", intervalMs≥60000} |
//           {kind:"cron", expr:"0 9 * * 1-5"} (локальное время).
//
// При срабатывании в сессию летит БРИФ — самодостаточный промпт с историей
// прошлого прогона (переживает компакшн). Доставка — session.prompt (queue:
// агент занят — промпт ждёт). Пропущенный за простой триггер догоняется раз.
// session.deleted → задачи сессии умирают.
//
// Инстансов плагина несколько (по локации) — тикер у каждого, дубль
// предотвращается lastFiredAt в файле: fire был < пол-интервала назад — скип.
// Запись файла атомарная (tmp+rename).

const LOG_PATH = join(homedir(), ".local/share/opencode/log/cron-mode.log")
const TASKS_FILE = join(homedir(), ".config/opencode/cron-tasks.json")
const TICK_MS = 30_000
const RESULT_CAP = 500
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
const MIN_TASK_MS = 60_000

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

// ——— файл задач ———

interface RawTask {
  id?: unknown
  name?: unknown
  sessionID?: unknown
  prompt?: unknown
  schedule?: unknown
  in?: unknown
  at?: unknown
  every?: unknown
  cron?: unknown
  createdAt?: unknown
  createdBy?: unknown
  lastFiredAt?: unknown
  lastResult?: unknown
  fireCount?: unknown
}

/** Разбор schedule-объекта из файла: {kind, at|intervalMs|expr}. */
function parseScheduleObject(s: unknown): Schedule | null {
  if (typeof s !== "object" || s === null) return null
  const o = s as { kind?: unknown; at?: unknown; intervalMs?: unknown; expr?: unknown }
  if (o.kind === "once" && typeof o.at === "number" && Number.isFinite(o.at) && o.at > 0) {
    return { kind: "once", at: o.at }
  }
  if (o.kind === "every" && typeof o.intervalMs === "number" && Number.isFinite(o.intervalMs) && o.intervalMs >= MIN_TASK_MS) {
    return { kind: "every", intervalMs: o.intervalMs }
  }
  if (o.kind === "cron" && typeof o.expr === "string" && parseCronFields(o.expr) !== null) {
    return { kind: "cron", expr: o.expr }
  }
  return null
}

function readTasks(): CronTask[] {
  let raw: unknown
  try {
    raw = JSON.parse(readFileSync(TASKS_FILE, "utf-8"))
  } catch {
    return [] // файла нет или битый — тик молча пуст
  }
  const arr: unknown = (raw as { tasks?: unknown } | undefined)?.tasks
  if (!Array.isArray(arr)) return []
  const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined)
  const tasks: CronTask[] = []
  let dirty = false
  const ids = new Set<string>()
  for (const r of arr as RawTask[]) {
    if (typeof r !== "object" || r === null) continue
    if (typeof r.prompt !== "string" || r.prompt.trim().length < 3) continue
    if (typeof r.sessionID !== "string" || !r.sessionID.startsWith("ses_")) continue
    let schedule: Schedule | null = parseScheduleObject(r.schedule)
    if (schedule === null) {
      schedule = parseSchedule(
        { in: str(r.in), at: str(r.at), every: str(r.every), cron: str(r.cron) },
        Date.now(),
      )
    }
    if (schedule === null) continue
    let id = typeof r.id === "string" ? r.id : ""
    if (!/^[0-9a-f]{4,8}$/.test(id)) {
      id = newId((x) => ids.has(x))
      dirty = true
    }
    if (ids.has(id)) continue
    ids.add(id)
    const createdAt = typeof r.createdAt === "number" ? r.createdAt : Date.now()
    if (typeof r.createdAt !== "number") dirty = true
    tasks.push({
      id,
      name: typeof r.name === "string" && r.name.trim() !== "" ? r.name : r.prompt.slice(0, 40),
      sessionID: r.sessionID,
      prompt: r.prompt.trim(),
      schedule,
      createdAt,
      createdBy: typeof r.createdBy === "string" ? r.createdBy : "агент",
      lastFiredAt: typeof r.lastFiredAt === "number" ? r.lastFiredAt : undefined,
      lastResult: typeof r.lastResult === "string" ? r.lastResult : undefined,
      fireCount: typeof r.fireCount === "number" ? r.fireCount : 0,
    })
  }
  if (dirty) writeTasks(tasks)
  return tasks
}

function writeTasks(tasks: CronTask[]): void {
  try {
    mkdirSync(join(TASKS_FILE, ".."), { recursive: true })
    const tmp = `${TASKS_FILE}.tmp`
    writeFileSync(tmp, JSON.stringify({ tasks }, null, 2))
    renameSync(tmp, TASKS_FILE)
  } catch (e) { log(`write tasks: ${e}`) }
}

// ——— плагин ———

export default Plugin.define({
  id: "cron-mode",
  setup: async (ctx) => {
    const firing = new Set<string>()

    const fire = async (task: CronTask, note = ""): Promise<void> => {
      if (firing.has(task.id)) return
      firing.add(task.id)
      try {
        task.lastResult = compactResult(await lastAssistantText(task.sessionID), RESULT_CAP)
        const brief = renderBrief(task, Date.now()) + (note ? `\n(${note})` : "")
        await ctx.session.prompt({ sessionID: task.sessionID, text: brief })
        task.fireCount++
        task.lastFiredAt = Date.now()
        task.lastResult = ""
        log(`fire #${task.id} → ${task.sessionID}`)
      } catch (e) { log(`fire #${task.id}: ${e}`) }
      finally { firing.delete(task.id) }
    }

    const tick = async (): Promise<void> => {
      const tasks = readTasks()
      if (tasks.length === 0) return
      const now = Date.now()
      let changed = false
      for (const task of tasks) {
        // анти-дубль между инстансами: чей-то свежий fire виден в файле
        const halfMs = task.schedule.kind === "every" ? task.schedule.intervalMs / 2 : 30_000
        if (task.lastFiredAt !== undefined && now - task.lastFiredAt < halfMs) continue
        const next = nextFire(task.schedule, task.lastFiredAt ?? task.createdAt)
        if (next === null || now < next) continue
        await fire(task)
        changed = true
      }
      const alive = tasks.filter((t) => t.schedule.kind !== "once" || t.lastFiredAt === undefined)
      if (changed) writeTasks(alive)
    }

    // catch-up при старте: просроченные за простой — один раз
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
            const tasks = readTasks()
            const alive = tasks.filter((t) => t.sessionID !== sid)
            if (alive.length === tasks.length) continue
            writeTasks(alive)
            log(`session ${sid} удалена — снято задач: ${tasks.length - alive.length}`)
          }
        } catch (e) {
          log(`event stream: ${e} — переподписка через 5с`)
        }
        if (eventsAbort.signal.aborted) return
        await new Promise((r) => setTimeout(r, 5_000))
      }
    })()

    log(`cron-mode: on (файл: ${TASKS_FILE})`)

    return () => {
      clearInterval(timer)
      eventsAbort.abort()
      log("cron-mode: off")
    }
  },
})
