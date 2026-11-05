// dreamer-lib.ts — чистые функции uwuwu-dreamer: кандидаты на анализ из
// opencode.db, state-машина прогонов (new/revived/retry → done/failed/gave_up),
// рестарт-реконсилиация, промпт дримеру. SQLite — bun:sqlite, всегда readonly.

import type { Database } from "bun:sqlite"
import { E2E_SESSION_PREFIX } from "../lib/e2e-tag"

/** LIKE-паттерн исключения e2e-сессий — строится из константы конвенции,
 *  литерал `[e2e]` в SQL не дублируется. */
const E2E_SKIP_LIKE = `${E2E_SESSION_PREFIX}%`

// ——— типы ———

export interface SessionRow {
  id: string
  title: string | null
  directory: string | null
  agent: string | null
  parentId: string | null
  timeCreated: number
  timeUpdated: number
}

export type ProcStatus = "pending" | "running" | "done" | "failed" | "gave_up" | "skipped"

export interface TrackedSession {
  status: ProcStatus
  /** time_updated сессии на момент спавна; рост поля = оживание → новый прогон */
  lastProcessedUpdate: number
  attempts: number
  /** сессия дримера, обслуживающая текущий/последний прогон */
  dreamerSes: string | null
  /** момент последнего wakeup-промпта; 0 = не было (критерий ответа: после него) */
  wokenAt: number
  at: number
}

export interface DreamerState {
  /** sid → tracking; персистится как state.json (tmp + rename, с .bak) */
  sessions: Record<string, TrackedSession>
  /** первый старт отработан (backlog-катофф размечен) */
  bootstrapped: boolean
}

export const emptyState = (): DreamerState => ({ sessions: {}, bootstrapped: false })

/** Старый state без wokenAt нормализуется (0 = не было wakeup). */
export function normalizeTracked(v: Record<string, unknown>): TrackedSession | null {
  if (v === null || typeof v !== "object") return null
  const statuses = ["pending", "running", "done", "failed", "gave_up", "skipped"]
  if (typeof v.status !== "string" || !statuses.includes(v.status)) return null
  if (typeof v.lastProcessedUpdate !== "number" || typeof v.attempts !== "number"
    || typeof v.at !== "number") return null
  if (v.dreamerSes !== null && typeof v.dreamerSes !== "string") return null
  return {
    status: v.status as ProcStatus,
    lastProcessedUpdate: v.lastProcessedUpdate,
    attempts: v.attempts,
    dreamerSes: (v.dreamerSes as string | null) ?? null,
    wokenAt: typeof v.wokenAt === "number" ? v.wokenAt : 0,
    at: v.at,
  }
}

export function isValidTracked(v: unknown): v is TrackedSession {
  return normalizeTracked(v as Record<string, unknown>) !== null
}

// ——— пиковое окно провайдера ———

/** "15-19" → часы 15:00–19:00 (локальное время ноды); "" / невалид → нет окна.
 *  В окно новые запуски (spawn/wakeup) запрещены — очередь копится и
 *  разряжается после его конца; закрытие уже идущих прогонов не блокируется. */
export interface PeakWindow { startHour: number; endHour: number }

export function parsePeakWindow(raw: string | undefined): PeakWindow | null {
  if (raw === undefined) raw = "15-19" // дефолт: пиковое окно провайдера
  const m = /^(\d{1,2})-(\d{1,2})$/.exec(raw.trim())
  if (m === null) return null
  const startHour = Number(m[1]), endHour = Number(m[2])
  if (startHour > 23 || endHour > 23) return null
  return { startHour, endHour }
}

export function isInPeakWindow(date: Date, win: PeakWindow): boolean {
  const h = date.getHours()
  return win.startHour <= win.endHour
    ? h >= win.startHour && h < win.endHour
    : h >= win.startHour || h < win.endHour // окно через полночь
}

// ——— выборка кандидатов ———

// Завершённость детектим по time_updated + порогу: time_idle не сбрасывается
// при оживлении сессии, idle_outcome NULL у ~91% строк — оба поля непригодны
// как сигнал «сессия закончилась». Молчание N минут — единственный надёжный.
export function idleBefore(nowMs: number, idleMs: number): number {
  return nowMs - idleMs
}

export function fetchRootSessions(db: Database, beforeMs: number, limit = 200): SessionRow[] {
  // Ревью-сессии фабрики (code-quality-reviewer) в пуле опыта наравне с
  // девелоперскими — осознанная отмена границы ADR-008 владельцем
  // (uwuwu: «Дример: работа с сессиями ревьюверов», строка 20261104215725-7zngmlj).
  // Исключён только сам dreamer — его прогоны не анализируют сами себя.
  // Помеченные e2e-сессии (конвенция lib/e2e-tag.ts) — синтетика, не пост-
  // обрабатывается: риг 04.11 без маркера породил 30 прогонов на 31 смоке.
  const rows = db
    .query(
      `SELECT id, title, directory, agent, parent_id, time_created, time_updated
       FROM session_v2
       WHERE COALESCE(agent, '') != 'dreamer'
         AND parent_id IS NULL
         AND time_updated < ?
         AND COALESCE(title, '') NOT LIKE ?
       ORDER BY time_updated DESC
       LIMIT ?`,
    )
    .all(beforeMs, E2E_SKIP_LIKE, limit) as Array<Record<string, unknown>>
  return rows.map(rowToSession)
}

/** Одна сессия по id (реконсилиация живых прогонов не зависит от idle-списка). */
export function fetchSessionById(db: Database, sid: string): SessionRow | null {
  const row = db
    .query(
      `SELECT id, title, directory, agent, parent_id, time_created, time_updated
       FROM session_v2 WHERE id = ?`,
    )
    .get(sid) as Record<string, unknown> | null
  return row === null ? null : rowToSession(row)
}

function rowToSession(r: Record<string, unknown>): SessionRow {
  return {
    id: String(r.id),
    title: r.title === null ? null : String(r.title),
    directory: r.directory === null ? null : String(r.directory),
    agent: r.agent === null ? null : String(r.agent),
    parentId: r.parent_id === null ? null : String(r.parent_id),
    timeCreated: Number(r.time_created),
    timeUpdated: Number(r.time_updated),
  }
}

export type CandidateReason = "new" | "revived" | "retry" | "stuck-pending"

/** Инвариант владельца: 1 рабочая сессия = 1 дример-сессия. Оживание/ретрай
 *  будят СУЩЕСТВУЮЩУЮ дример-сессию промптом (wakeup); spawn — только когда
 *  её нет (первый прогон / dreamerSes потеряна — демон верифицирует живость
 *  через REST перед промптом, либа от state детерминирована). */
export type CandidateAction = "spawn" | "wakeup"

export interface Candidate {
  session: SessionRow
  reason: CandidateReason
  action: CandidateAction
  /** revived-прогон сбрасывает счётчик попыток — это новый цикл работы */
  resetAttempts: boolean
}

/** Сессия уже покрыта прошлым прогоном (не оживала с тех пор). */
function isCovered(tracked: TrackedSession, session: SessionRow): boolean {
  if (tracked.status === "done" || tracked.status === "skipped") {
    return tracked.lastProcessedUpdate >= session.timeUpdated
  }
  return false
}

/**
 * Кандидаты на обработку: idle-корневые сессии, которые не обработаны /
 * ожили после прошлого прогона / не исчерпали ретраи. action: dreamerSes
 * есть → wakeup (та же сессия дримера — инвариант 1=1), нет → spawn.
 * не обработаны / ожили после прошлого прогона / не исчерпали ретраи.
 * Порядок входа — свежие вперёд (осознанно: недавняя сессия анализируется
 * точнее — контекст и git-стейт ещё актуальны; FIFO жертвовал бы свежестью).
 */
export function pickCandidates(
  sessions: SessionRow[],
  state: DreamerState,
  maxAttempts: number,
): Candidate[] {
  const out: Candidate[] = []
  const action = (dreamerSes: string | null): CandidateAction =>
    dreamerSes !== null ? "wakeup" : "spawn"
  for (const session of sessions) {
    const tracked = state.sessions[session.id]
    if (tracked === undefined) {
      out.push({ session, reason: "new", action: "spawn", resetAttempts: false })
      continue
    }
    if (isCovered(tracked, session)) continue
    switch (tracked.status) {
      case "running":
        break // живой прогон — реконсилируется отдельно (applyRunOutcome)
      case "pending":
        if (tracked.dreamerSes === null) {
          // краш демона между записью pending и спавном → повторный спавн
          out.push({ session, reason: "stuck-pending", action: "spawn", resetAttempts: false })
        }
        break
      case "failed":
        if (tracked.attempts < maxAttempts) {
          out.push({ session, reason: "retry", action: action(tracked.dreamerSes), resetAttempts: false })
        }
        break
      case "done":
      case "skipped":
      case "gave_up":
        // не covered → сессия ожила: новый цикл, попытки с нуля
        // (gave_up — не навсегда: новая активность сессии заслуживает нового прогона)
        out.push({ session, reason: "revived", action: action(tracked.dreamerSes), resetAttempts: true })
        break
    }
  }
  return out
}

// ——— backlog-катофф ———

/**
 * Первый старт демона на ноде: сессии, уснувшие раньше cutoff, не гоняем —
 * ретроспектива всей истории дорога и бесполезна. Полный проход без лимита
 * (разовая операция, дёшево даже на большой истории) — иначе свежие сессии
 * за окном потоковой выборки никогда не размечаются.
 */
export function markBacklog(db: Database, state: DreamerState, cutoffMs: number): number {
  const rows = db
    .query(
      `SELECT id, time_updated FROM session_v2
       WHERE COALESCE(agent, '') != 'dreamer'
         AND parent_id IS NULL
         AND time_updated < ?
         AND COALESCE(title, '') NOT LIKE ?`,
    )
    .all(cutoffMs, E2E_SKIP_LIKE) as Array<{ id: string; time_updated: number }>
  let marked = 0
  for (const row of rows) {
    const sid = String(row.id)
    if (state.sessions[sid] === undefined) {
      state.sessions[sid] = {
        status: "skipped",
        lastProcessedUpdate: Number(row.time_updated),
        attempts: 0,
        dreamerSes: null,
        wokenAt: 0,
        at: Date.now(),
      }
      marked++
    }
  }
  return marked
}

// ——— завершение прогонов ———

export interface RunOutcome {
  /** сессия дримера отвечала (есть assistant-сообщение) — работа сделана */
  replied: boolean
  /** сессия дримера исчезла из opencode (404/400) */
  dead: boolean
  /** сессия жива, но молчит дольше таймаута прогона */
  timedOut: boolean
}

/** Переход state-машины по исходу проверки running-прогона. */
export function applyRunOutcome(
  tracked: TrackedSession,
  session: SessionRow,
  outcome: RunOutcome,
  maxAttempts: number,
): "done" | "failed" | "gave_up" | "keep" {
  if (!outcome.dead && !outcome.timedOut) return "keep"
  if (outcome.replied) {
    tracked.status = "done"
    tracked.lastProcessedUpdate = session.timeUpdated
    tracked.at = Date.now()
    return "done"
  }
  tracked.attempts += 1
  tracked.status = tracked.attempts >= maxAttempts ? "gave_up" : "failed"
  tracked.at = Date.now()
  return tracked.status
}

/** Есть ли в сессии дримера ответ ассистента (минимальное доказательство работы). */
export function hasAssistantReply(db: Database, dreamerSes: string): boolean {
  const row = db
    .query(`SELECT COUNT(*) AS n FROM session_message WHERE session_id = ? AND type = 'assistant'`)
    .get(dreamerSes) as { n: number } | null
  return (row?.n ?? 0) > 0
}

/** Ответ ассистента ПОЗЖЕ границы (wakeup: старые ответы — прошлый прогон). */
export function hasAssistantReplySince(db: Database, dreamerSes: string, sinceMs: number): boolean {
  const row = db
    .query(
      `SELECT COUNT(*) AS n FROM session_message
       WHERE session_id = ? AND type = 'assistant' AND time_created > ?`,
    )
    .get(dreamerSes, sinceMs) as { n: number } | null
  return (row?.n ?? 0) > 0
}

// ——— промпт ———

export interface PromptInput {
  sessionId: string
  sessionTitle: string | null
  auditPath: string
  wikiOpsPath: string
  dbPath: string
}

/**
 * Промпт демона дримеру — только входные данные; вся методика живёт в
 * agents/dreamer.md (единая точка правды, агент подхватит её сам).
 */
export function dreamerPrompt(input: PromptInput): string {
  const title = input.sessionTitle ?? "(без заголовка)"
  return [
    `Дример: проанализируй завершённую сессию ${input.sessionId} («${title}»).`,
    ``,
    `Входы:`,
    `- дамп сессии: python3 "${input.auditPath}" --db "${input.dbPath}" --compact session ${input.sessionId}`,
    `- БД опенкода (readonly, при необходимости прямых SELECT): ${input.dbPath}`,
    `- чтение вики: uwuwu-cli read "<slug>" (живое); поиск: uwuwu-cli search`,
    `- запись в вики (абсолютный путь — cwd у тебя чужой проект): "${input.wikiOpsPath}"`,
    ``,
    `Действуй строго по своей методике. Заверши одной-двумя строками: что`,
    `записал (тайтлы) / «нечего взять» / причина недоступности вики.`,
  ].join("\n")
}

/** Промпт повторного прогона: сессия ожила — будим ту же дример-сессию.
 *  Граница задаётся явно: дамп не фильтруется по времени, а compaction
 *  съедает детали старых прогонов — граница остаётся якорем новизны. */
export function wakeupPrompt(input: PromptInput & { sinceIso: string }): string {
  const title = input.sessionTitle ?? "(без заголовка)"
  return [
    `Дример: повторный прогон — сессия ${input.sessionId} («${title}») ожила после прошлого анализа.`,
    `Граница прошлого покрытия: ${input.sinceIso} (мс: ${Date.parse(input.sinceIso)}).`,
    `В дампе новое = сообщения с time_created выше границы; проанализируй дельту по своей методике.`,
    `Ты помнишь свои прошлые записи этой сессии — не дублируй их, дедуп по своей истории.`,
    ``,
    `Входы те же:`,
    `- дамп: python3 "${input.auditPath}" --db "${input.dbPath}" --compact session ${input.sessionId}`,
    `- чтение вики: uwuwu-cli read; запись: "${input.wikiOpsPath}"`,
    ``,
    `Заверши одной-двумя строками: что дополнительно записал / «в дельте нечего взять».`,
  ].join("\n")
}