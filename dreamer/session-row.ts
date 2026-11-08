// session-row.ts — базовая работа со строками session_v2 opencode.db:
// тип строки, row-маппер, выборка по id, LIKE-паттерн e2e-исключений.
// Отдельный модуль без контурной логики: посессионный (dreamer-lib) и
// задачный (task-run-lib) контуры оба работают с этими строками.
import type { Database } from "bun:sqlite"
import { E2E_SESSION_PREFIX } from "../lib/e2e-tag"

/** LIKE-паттерн исключения e2e-сессий — строится из константы конвенции,
 *  литерал `[e2e]` в SQL не дублируется. */
export const E2E_SKIP_LIKE = `${E2E_SESSION_PREFIX}%`

/** SQL-хвост исключений сессий дримера и e2e-синтетики — общий для всех
 *  выборок (посессионный пул, backlog-разметка, сбор сессий задачи).
 *  Плейсхолдер LIKE-паттерна — второй параметр вызова. */
export const SESSION_EXCLUSIONS_SQL =
  `COALESCE(agent, '') != 'dreamer' AND COALESCE(title, '') NOT LIKE ?`

export interface SessionRow {
  id: string
  title: string | null
  directory: string | null
  agent: string | null
  parentId: string | null
  timeCreated: number
  timeUpdated: number
}

export function rowToSession(r: Record<string, unknown>): SessionRow {
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

const SESSION_COLUMNS = `id, title, directory, agent, parent_id, time_created, time_updated`

/** Одна сессия по id (реконсил живых прогонов не зависит от idle-списка). */
export function fetchSessionById(db: Database, sid: string): SessionRow | null {
  const row = db
    .query(`SELECT ${SESSION_COLUMNS} FROM session_v2 WHERE id = ?`)
    .get(sid) as Record<string, unknown> | null
  return row === null ? null : rowToSession(row)
}

/** Дочерние сессии по родителям (один уровень BFS сбора задачи). */
export function fetchSessionsByParents(db: Database, parentIds: string[]): SessionRow[] {
  if (parentIds.length === 0) return []
  const placeholders = parentIds.map(() => "?").join(", ")
  const rows = db
    .query(
      `SELECT ${SESSION_COLUMNS} FROM session_v2
       WHERE parent_id IN (${placeholders}) AND ${SESSION_EXCLUSIONS_SQL}`,
    )
    .all(...parentIds, E2E_SKIP_LIKE) as Array<Record<string, unknown>>
  return rows.map(rowToSession)
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

/** time_updated сессии: живая, но молчащая дольше таймаута = зависла. */
export function sessionTimedOut(db: Database, sessionId: string, timeoutMs: number): boolean {
  const row = db
    .query(`SELECT time_updated FROM session_v2 WHERE id = ?`)
    .get(sessionId) as { time_updated: number } | null
  if (row === null) return false // сессии нет в БД — прогон закрыт реконсилиацией
  return Date.now() - row.time_updated > timeoutMs
}
