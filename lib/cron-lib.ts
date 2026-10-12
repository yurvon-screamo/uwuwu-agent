// cron-lib.ts — чистые функции cron-mode: расписания, next-fire, бриф.
// Без зависимостей — импортируется плагином, хабом и тестами.
// Все времена — локальные (системная таймзона, у нас Asia/Tokyo).

export interface CronTask {
  id: string
  name?: string
  sessionID: string
  /** Самодостаточная инструкция: что делать, что с результатом, критерий снятия. */
  prompt: string
  schedule: Schedule
  createdAt: number
  createdBy?: string
  lastFiredAt?: number
  lastResult?: string
  fireCount: number
}

export type Schedule =
  | { kind: "once"; at: number }
  | { kind: "every"; intervalMs: number }
  | { kind: "cron"; expr: string }

export type ScheduleInput = {
  /** Однораз: через N — "20m", "90s", "2h". */
  in?: string
  /** Однораз: ближайшее локальное время "HH:MM". */
  at?: string
  /** Постоянно: интервал "30m", "2h", "1d". */
  every?: string
  /** Постоянно: cron 5 полей, локальная таймзона. */
  cron?: string
}

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** "90s" | "20m" | "2h" | "1d" → мс. null если не распознано. */
export function parseDuration(s: string): number | null {
  const m = s.trim().match(/^(\d+)\s*(s|m|h|d)$/i)
  if (!m) return null
  const n = Number(m[1])
  const unit = m[2].toLowerCase()
  if (!Number.isFinite(n) || n <= 0) return null
  if (unit === "s") return n * 1000
  if (unit === "m") return n * MINUTE
  if (unit === "h") return n * HOUR
  return n * DAY
}

/** "HH:MM" → {h, m}. null если не распознано. */
export function parseHHMM(s: string): { h: number; m: number } | null {
  const match = s.trim().match(/^(\d{1,2}):(\d{2})$/)
  if (!match) return null
  const h = Number(match[1])
  const mi = Number(match[2])
  if (h > 23 || mi > 59) return null
  return { h, m: mi }
}

/** Разбор ввода в Schedule. null — ничего не распознано / конфликт. */
export function parseSchedule(input: ScheduleInput, now: number): Schedule | null {
  const filled = Object.values(input).filter((v) => v !== undefined).length
  if (filled !== 1) return null // ровно один способ задать время

  if (input.in !== undefined) {
    const ms = parseDuration(input.in)
    if (ms === null) return null
    return { kind: "once", at: now + ms }
  }
  if (input.at !== undefined) {
    const hm = parseHHMM(input.at)
    if (hm === null) return null
    const d = new Date(now)
    const at = new Date(d.getFullYear(), d.getMonth(), d.getDate(), hm.h, hm.m, 0, 0).getTime()
    // если время уже прошло сегодня — завтра
    return { kind: "once", at: at > now ? at : at + DAY }
  }
  if (input.every !== undefined) {
    const ms = parseDuration(input.every)
    if (ms === null || ms < MINUTE) return null // постоянка короче минуты не бывает
    return { kind: "every", intervalMs: ms }
  }
  if (input.cron !== undefined) {
    return parseCronFields(input.cron) ? { kind: "cron", expr: input.cron } : null
  }
  return null
}

// ——— cron 5 полей: m h dom mon dow ———

type Field = { values: Set<number>; wildcard: boolean }

export function parseCronFields(expr: string): { min: Field; hour: Field; dom: Field; mon: Field; dow: Field } | null {
  const parts = expr.trim().split(/\s+/)
  if (parts.length !== 5) return null
  const ranges = [
    { min: 0, max: 59 },
    { min: 0, max: 23 },
    { min: 1, max: 31 },
    { min: 1, max: 12 },
    { min: 0, max: 7 }, // 0 и 7 = воскресенье
  ]
  const fields: Field[] = []
  for (let i = 0; i < 5; i++) {
    const f = parseCronField(parts[i], ranges[i].min, ranges[i].max)
    if (f === null) return null
    if (i === 4 && f.values.has(7)) f.values.add(0) // 7 → воскресенье (0 уже есть)
    fields.push(f)
  }
  return { min: fields[0], hour: fields[1], dom: fields[2], mon: fields[3], dow: fields[4] }
}

function parseCronField(field: string, min: number, max: number): Field | null {
  const values = new Set<number>()
  let wildcard = false
  for (const part of field.split(",")) {
    const [rangePart, stepPart] = part.split("/")
    const step = stepPart === undefined ? 1 : Number(stepPart)
    if (!Number.isInteger(step) || step < 1) return null
    if (rangePart === "*") {
      wildcard = true
      for (let v = min; v <= max; v += step) values.add(v)
      continue
    }
    const dash = rangePart.split("-")
    const a = Number(dash[0])
    const b = dash.length === 2 ? Number(dash[1]) : a
    if (!Number.isInteger(a) || !Number.isInteger(b) || a < min || b > max || a > b) return null
    for (let v = a; v <= b; v += step) values.add(v)
  }
  if (values.size === 0) return null
  return { values, wildcard }
}

/** Следующая минута, удовлетворяющая cron (vixie: dom и dow — OR если оба ограничены). */
export function nextCronFire(expr: string, from: number): number | null {
  const f = parseCronFields(expr)
  if (f === null) return null
  const d = new Date(from + MINUTE)
  d.setSeconds(0, 0)
  const limit = from + 366 * DAY
  while (d.getTime() <= limit) {
    const mon = d.getMonth() + 1
    const dom = d.getDate()
    const dow = d.getDay()
    const dayOk = f.mon.values.has(mon) && (f.dom.wildcard || f.dow.wildcard
      ? f.dom.values.has(dom) && f.dow.values.has(dow)
      : f.dom.values.has(dom) || f.dow.values.has(dow))
    if (dayOk && f.hour.values.has(d.getHours()) && f.min.values.has(d.getMinutes())) {
      return d.getTime()
    }
    d.setTime(d.getTime() + MINUTE)
  }
  return null
}

/** Следующий момент срабатывания задачи. null — расписание сломано. */
export function nextFire(schedule: Schedule, from: number): number | null {
  if (schedule.kind === "once") return schedule.at
  if (schedule.kind === "cron") return nextCronFire(schedule.expr, from)
  return from + schedule.intervalMs
}

/** Формат локального времени "ДД.ММ HH:MM". */
export function fmtLocal(ms: number): string {
  const d = new Date(ms)
  const p = (n: number): string => String(n).padStart(2, "0")
  return `${p(d.getDate())}.${p(d.getMonth() + 1)} ${p(d.getHours())}:${p(d.getMinutes())}`
}

/** Компактный хвост результата для брифа. */
export function compactResult(text: string, cap = 500): string {
  const t = text.replace(/\s+/g, " ").trim()
  return t.length > cap ? `${t.slice(0, cap - 1)}…` : t
}

/** Человекочитаемое расписание. */
export function describeSchedule(schedule: Schedule, from = Date.now()): string {
  if (schedule.kind === "once") return `однократно, ${fmtLocal(schedule.at)}`
  if (schedule.kind === "every") {
    const m = schedule.intervalMs / MINUTE
    if (m < 60) return `каждые ${Math.round(m)} мин`
    if (m < 60 * 24) return `каждые ${Math.round(m / 60)} ч`
    return `каждые ${Math.round(m / (60 * 24))} сут`
  }
  const next = nextCronFire(schedule.expr, from)
  const nextStr = next === null ? "" : ` (след. ${fmtLocal(next)})`
  return `cron «${schedule.expr}»${nextStr}`
}

/** Бриф при срабатывании: самодостаточный, с историей и выходом. */
export function renderBrief(task: CronTask, now: number): string {
  const lines = [
    `[cron #${task.id}] ${task.name ?? "задача"} · ${describeSchedule(task.schedule, now)}`,
    `Поставлена: ${fmtLocal(task.createdAt)}${task.createdBy ? ` (${task.createdBy})` : ""} · прогонов: ${task.fireCount}`,
    "",
    "ДЕЙСТВИЕ:",
    task.prompt,
  ]
  if (task.lastResult) {
    lines.push("", `Прошлый прогон (${task.lastFiredAt ? fmtLocal(task.lastFiredAt) : "—"}): ${task.lastResult}`)
  }
  lines.push("", "Если задача утратила смысл — сними её тулом cron_remove (id выше) и коротко поясни почему.")
  return lines.join("\n")
}

/** Короткое id: 4 hex-символа. */
export function newId(exists: (id: string) => boolean): string {
  for (let i = 0; i < 100; i++) {
    const id = Math.floor(Math.random() * 0xffff).toString(16).padStart(4, "0")
    if (!exists(id)) return id
  }
  return String(Date.now() % 0xffff)
}

/** Ручная валидация ввода cron_set (JSON Schema описывает, это проверяет). */
export function validateTaskInput(input: { prompt?: unknown } & ScheduleInput): { ok: true; prompt: string } | { ok: false; error: string } {
  if (typeof input.prompt !== "string" || input.prompt.trim().length < 3) {
    return { ok: false, error: "prompt обязателен (мин. 3 символа) — самодостаточная инструкция" }
  }
  return { ok: true, prompt: input.prompt.trim() }
}

/** Пиковые часы (будни 15:00–19:00 JST) — soft-предупреждение в подтверждении. */
export function peakHoursWarning(fireAt: number): string {
  const d = new Date(fireAt)
  const dow = d.getDay()
  const h = d.getHours()
  if (dow >= 1 && dow <= 5 && h >= 15 && h < 19) {
    return "⚠️ попадает в пиковые часы (будни 15:00–19:00) — по возможности ставь вне их"
  }
  return ""
}
