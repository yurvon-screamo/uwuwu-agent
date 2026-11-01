// dreamer-lib.ts — чистые функции uwuwu-dreamer: кандидаты на анализ из
// opencode.db, state-машина прогонов (new/revived/retry → done/failed/gave_up),
// рестарт-реконсилиация, промпт дримеру. SQLite — bun:sqlite, всегда readonly.

import type { Database } from "bun:sqlite"

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
  at: number
}

export interface DreamerState {
  /** sid → tracking; персистится как state.json (tmp + rename, с .bak) */
  sessions: Record<string, TrackedSession>
  /** первый старт отработан (backlog-катофф размечен) */
  bootstrapped: boolean
}

export const emptyState = (): DreamerState => ({ sessions: {}, bootstrapped: false })

export function isValidTracked(v: unknown): v is TrackedSession {
  if (v === null || typeof v !== "object") return false
  const t = v as Record<string, unknown>
  const statuses = ["pending", "running", "done", "failed", "gave_up", "skipped"]
  return typeof t.status === "string" && statuses.includes(t.status)
    && typeof t.lastProcessedUpdate === "number"
    && typeof t.attempts === "number"
    && (t.dreamerSes === null || typeof t.dreamerSes === "string")
    && typeof t.at === "number"
}

// ——— выборка кандидатов ———

// Завершённость детектим по time_updated + порогу: time_idle не сбрасывается
// при оживлении сессии, idle_outcome NULL у ~91% строк — оба поля непригодны
// как сигнал «сессия закончилась». Молчание N минут — единственный надёжный.
export function idleBefore(nowMs: number, idleMs: number): number {
  return nowMs - idleMs
}

export function fetchRootSessions(db: Database, beforeMs: number, limit = 200): SessionRow[] {
  const rows = db
    .query(
      `SELECT id, title, directory, agent, parent_id, time_created, time_updated
       FROM session_v2
       WHERE COALESCE(agent, '') != 'dreamer'
         AND parent_id IS NULL
         AND time_updated < ?
       ORDER BY time_updated DESC
       LIMIT ?`,
    )
    .all(beforeMs, limit) as Array<Record<string, unknown>>
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

export interface Candidate {
  session: SessionRow
  reason: CandidateReason
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
 * Кандидаты на спавн дримера: idle-корневые сессии, которые
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
  for (const session of sessions) {
    const tracked = state.sessions[session.id]
    if (tracked === undefined) {
      out.push({ session, reason: "new", resetAttempts: false })
      continue
    }
    if (isCovered(tracked, session)) continue
    switch (tracked.status) {
      case "running":
        break // живой прогон — реконсилируется отдельно (applyRunOutcome)
      case "pending":
        if (tracked.dreamerSes === null) {
          // краш демона между записью pending и спавном → повторный спавн
          out.push({ session, reason: "stuck-pending", resetAttempts: false })
        }
        break
      case "failed":
        if (tracked.attempts < maxAttempts) {
          out.push({ session, reason: "retry", resetAttempts: false })
        }
        break
      case "done":
      case "skipped":
      case "gave_up":
        // не covered → сессия ожила: новый цикл, попытки с нуля
        // (gave_up — не навсегда: новая активность сессии заслуживает нового прогона)
        out.push({ session, reason: "revived", resetAttempts: true })
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
         AND time_updated < ?`,
    )
    .all(cutoffMs) as Array<{ id: string; time_updated: number }>
  let marked = 0
  for (const row of rows) {
    const sid = String(row.id)
    if (state.sessions[sid] === undefined) {
      state.sessions[sid] = {
        status: "skipped",
        lastProcessedUpdate: Number(row.time_updated),
        attempts: 0,
        dreamerSes: null,
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
    `- дамп сессии: python3 "${input.auditPath}" session ${input.sessionId} --compact --db "${input.dbPath}"`,
    `- БД опенкода (readonly, при необходимости прямых SELECT): ${input.dbPath}`,
    `- чтение вики: uwuwu-cli read "<slug>" (живое); поиск: uwuwu-cli search`,
    `- запись в вики (абсолютный путь — cwd у тебя чужой проект): "${input.wikiOpsPath}"`,
    ``,
    `Действуй строго по своей методике. Заверши одной-двумя строками: что`,
    `записал (тайтлы) / «нечего взять» / причина недоступности вики.`,
  ].join("\n")
}
