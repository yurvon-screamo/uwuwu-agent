// session-row.ts — dreamer-контур над базовыми строками session_v2
// (базовый слой: тип строки, row-маппер, выборки — в lib/session-row.ts):
// e2e-исключения, выборки по родителям, доказательства работы ассистента.
import type { Database } from "bun:sqlite"
import { E2E_SESSION_PREFIX } from "../lib/e2e-tag"
import { SESSION_COLUMNS, rowToSession, type SessionRow } from "../lib/session-row"

export { fetchSessionById, rowToSession, type SessionRow } from "../lib/session-row"

/** LIKE-паттерн исключения e2e-сессий — строится из константы конвенции,
 *  литерал `[e2e]` в SQL не дублируется. */
export const E2E_SKIP_LIKE = `${E2E_SESSION_PREFIX}%`

/** SQL-хвост исключений сессий дримера и e2e-синтетики — общий для всех
 *  выборок (посессионный пул, backlog-разметка, сбор сессий задачи).
 *  Плейсхолдер LIKE-паттерна — второй параметр вызова. */
export const SESSION_EXCLUSIONS_SQL =
  `COALESCE(agent, '') != 'dreamer' AND COALESCE(title, '') NOT LIKE ?`

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
