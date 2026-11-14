// session-row.test.ts — юниты базового слоя session_v2 (задача 1072):
// выборка по id и по каталогу на tmp-БД с минимальной схемой session_v2.
import { describe, expect, test } from "bun:test"
import { Database } from "bun:sqlite"
import { existsSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { fetchSessionById, fetchLatestSessionByDirectory } from "./session-row"

/** Минимальная схема session_v2 (колонки базового слоя; прод-схема шире —
 *  читалка пинится этими колонками через SESSION_COLUMNS). */
function createSessionDb(): Database {
  const db = new Database(":memory:")
  db.exec(`CREATE TABLE session_v2 (
    id text PRIMARY KEY,
    title text,
    directory text,
    agent text,
    parent_id text,
    time_created integer NOT NULL,
    time_updated integer NOT NULL
  )`)
  return db
}

function insertSession(
  db: Database,
  id: string,
  directory: string,
  timeUpdated: number,
): void {
  db.query(
    `INSERT INTO session_v2 (id, title, directory, agent, parent_id, time_created, time_updated)
     VALUES (?, ?, ?, NULL, NULL, ?, ?)`,
  ).run(id, `тема ${id}`, directory, timeUpdated - 1000, timeUpdated)
}

describe("fetchSessionById: сессия по id (1072, id-ключ dead-гарда)", () => {
  test("существующая строка маппится в SessionRow (timeUpdated числом)", () => {
    const db = createSessionDb()
    try {
      insertSession(db, "ses_a", "/env/1/repo", 1000)
      const row = fetchSessionById(db, "ses_a")
      expect(row?.id).toBe("ses_a")
      expect(row?.directory).toBe("/env/1/repo")
      expect(row?.timeUpdated).toBe(1000)
    } finally {
      db.close()
    }
  })

  test("нет строки — null (следа нет → stale)", () => {
    const db = createSessionDb()
    try {
      expect(fetchSessionById(db, "ses_missing")).toBeNull()
    } finally {
      db.close()
    }
  })
})

describe("fetchLatestSessionByDirectory: свежайшая сессия каталога (1072, directory-ключ)", () => {
  test("возвращает самую свежую по time_updated среди сессий каталога", () => {
    const db = createSessionDb()
    try {
      insertSession(db, "ses_old", "/env/317/repo", 1000)
      insertSession(db, "ses_new", "/env/317/repo", 5000)
      insertSession(db, "ses_other", "/env/999/repo", 9000)
      const row = fetchLatestSessionByDirectory(db, "/env/317/repo")
      expect(row?.id).toBe("ses_new")
      expect(row?.timeUpdated).toBe(5000)
    } finally {
      db.close()
    }
  })

  test("точный матч каталога: чужой каталог не попадает", () => {
    const db = createSessionDb()
    try {
      insertSession(db, "ses_x", "/env/317/repo", 1000)
      expect(fetchLatestSessionByDirectory(db, "/env/318/repo")).toBeNull()
    } finally {
      db.close()
    }
  })

  test("сессий каталога нет — null", () => {
    const db = createSessionDb()
    try {
      expect(fetchLatestSessionByDirectory(db, "/env/empty/repo")).toBeNull()
    } finally {
      db.close()
    }
  })
})

// ——— живая opencode.db: прод-схема session_v2 (гвард UWUWU_E2E=1) ———
// Минимальная фикстура выше пинит логику выборок; живая БД пинит реальную
// схему таблицы — дрейф схемы (переименование/тип колонки) ловится здесь,
// а не неизвестной деградацией гарда dead-спавна в проде (1072). Read-only
// коннект — тот же вызов, что у factory.withSessionDb; гвард по культуре
// репо (qlty-review.test.ts): без UWUWU_E2E=1 и файла БД сразу зелёный.
const liveDbPath = join(homedir(), ".local/share/opencode/opencode.db")
const liveE2E = process.env.UWUWU_E2E === "1" && existsSync(liveDbPath)

describe("живая opencode.db — прод-схема session_v2 (UWUWU_E2E=1)", () => {
  test.skipIf(!liveE2E)(
    "id- и directory-выборки парсят живую строку реальной схемы",
    () => {
      const db = new Database(liveDbPath, { readonly: true })
      try {
        const anyRow = db
          .query("SELECT id, directory FROM session_v2 ORDER BY time_updated DESC LIMIT 1")
          .get() as { id: string; directory: string | null } | null
        if (anyRow === null) return // пустая БД — пинить нечего
        const byId = fetchSessionById(db, anyRow.id)
        expect(byId?.id).toBe(anyRow.id)
        expect(typeof byId?.timeUpdated).toBe("number")
        expect(typeof byId?.timeCreated).toBe("number")
        if (anyRow.directory !== null) {
          const byDir = fetchLatestSessionByDirectory(db, anyRow.directory)
          expect(byDir?.directory).toBe(anyRow.directory)
        }
      } finally {
        db.close()
      }
    },
  )
})
