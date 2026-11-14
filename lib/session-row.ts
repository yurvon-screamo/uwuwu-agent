// session-row.ts — базовая работа со строками session_v2 opencode.db:
// тип строки, row-маппер, выборка по id и по каталогу. Общий модуль без
// контурной логики: dreamer (посессионный пул, реконсил) и factory (гард
// dead-спавна по БД-следу, задача 1072) оба работают с этими строками.
import type { Database } from "bun:sqlite"

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

export const SESSION_COLUMNS = `id, title, directory, agent, parent_id, time_created, time_updated`

/** Одна сессия по id (реконсил живых прогонов не зависит от idle-списка). */
export function fetchSessionById(db: Database, sid: string): SessionRow | null {
  const row = db
    .query(`SELECT ${SESSION_COLUMNS} FROM session_v2 WHERE id = ?`)
    .get(sid) as Record<string, unknown> | null
  return row === null ? null : rowToSession(row)
}

/** Свежайшая сессия каталога (гард dead-спавна: работа в env задачи —
 *  задача 1072; null — сессий каталога в БД нет). */
export function fetchLatestSessionByDirectory(db: Database, directory: string): SessionRow | null {
  const row = db
    .query(`SELECT ${SESSION_COLUMNS} FROM session_v2 WHERE directory = ? ORDER BY time_updated DESC LIMIT 1`)
    .get(directory) as Record<string, unknown> | null
  return row === null ? null : rowToSession(row)
}
