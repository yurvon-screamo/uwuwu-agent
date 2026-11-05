// tests/platform-pagination.test.ts — контракт полноты выборки доски.
// Серверный pageSize database render — 50: один вызов отдаёт только первую
// страницу (живая проба 05.11: rowCount=145, в ответе ровно 50) — адаптер
// обязан пагинировать. Юниты цикла — на фейковых страницах; живой тест —
// против реальной доски «Задачи uwuwu» с независимой пробой rowCount.
import { describe, expect, test } from "bun:test"
import {
  MAX_RENDER_PAGES,
  collectBoardView,
  type BoardViewPage,
  type FetchBoardPage,
} from "../lib/board-view"
import { tool, __test } from "../lib/platform"

const BOARD_TITLE = "Задачи uwuwu"

function pageOf(rows: string[], rowCount?: number): BoardViewPage {
  return { view: { rowCount, rows: rows.map((id) => ({ id })) } }
}

describe("collectBoardView: пагинация", () => {
  test("мультистраница — собирает страницы до счётчика ядра", async () => {
    // Arrange
    const pages: Record<number, BoardViewPage> = {
      1: { view: { columns: [{ id: "c1", name: "Status" }], rowCount: 4, rows: [{ id: "a" }, { id: "b" }] } },
      2: { view: { rows: [{ id: "c" }, { id: "d" }] } },
    }
    const requested: number[] = []
    const fetchPage: FetchBoardPage = async (page) => {
      requested.push(page)
      return pages[page] ?? { view: { rows: [] } }
    }

    // Act
    const view = await collectBoardView(fetchPage)

    // Assert
    expect(view.rows.map((r) => r.id)).toEqual(["a", "b", "c", "d"])
    expect(view.columns).toEqual([{ id: "c1", name: "Status" }])
    expect(requested).toEqual([1, 2])
  })

  test("rowCount устарел вниз — терминация по пустой странице, без капа", async () => {
    // Arrange: счётчик promise 1000, по факту строк всего 2
    const pages: Record<number, BoardViewPage> = {
      1: { view: { rowCount: 1000, rows: [{ id: "a" }, { id: "b" }] } },
      2: { view: { rows: [] } },
    }

    // Act
    const view = await collectBoardView(async (page) => pages[page] ?? { view: { rows: [] } })

    // Assert
    expect(view.rows.map((r) => r.id)).toEqual(["a", "b"])
  })

  test("rowCount 0/отсутствует — терминирует пустая страница", async () => {
    // Arrange
    const pages: Record<number, BoardViewPage> = {
      1: { view: { rows: [{ id: "a" }] } },
      2: { view: { rows: [] } },
    }

    // Act
    const view = await collectBoardView(async (page) => pages[page] ?? { view: { rows: [] } })

    // Assert
    expect(view.rows.map((r) => r.id)).toEqual(["a"])
  })

  test("дрейф страниц: дубль строки на стыке дедупится по id", async () => {
    // Arrange
    const pages: Record<number, BoardViewPage> = {
      1: { view: { rowCount: 3, rows: [{ id: "a" }, { id: "b" }] } },
      2: { view: { rows: [{ id: "b" }, { id: "c" }] } },
    }

    // Act
    const view = await collectBoardView(async (page) => pages[page] ?? { view: { rows: [] } })

    // Assert
    expect(view.rows.map((r) => r.id)).toEqual(["a", "b", "c"])
  })

  test("строки без id не дедупятся", async () => {
    // Arrange
    const pages: Record<number, BoardViewPage> = {
      1: { view: { rowCount: 2, rows: [{}, {}] } },
      2: { view: { rows: [] } },
    }

    // Act
    const view = await collectBoardView(async (page) => pages[page] ?? { view: { rows: [] } })

    // Assert
    expect(view.rows).toHaveLength(2)
  })

  test("кап страниц — громкая ошибка, не тихая усечка", async () => {
    // Arrange: вечно полные страницы, счётчик недостижим
    const fetchPage: FetchBoardPage = async () =>
      pageOf(Array.from({ length: 50 }, (_, i) => `r${i}`), 1_000_000)

    // Act / Assert
    expect(collectBoardView(fetchPage)).rejects.toThrow(/кап/)
  })

  test("кап — 100 страниц", () => {
    expect(MAX_RENDER_PAGES).toBe(100)
  })
})

describe("read_database_cells: полнота доски (живое ядро)", () => {
  const hasEnv = (process.env.UWUWU_SIYUAN_TOKEN ?? process.env.UWUWU_SIYUAN_WS ?? "") !== ""

  test.skipIf(!hasEnv)("адаптер видит ≥ rowCount строк доски «Задачи uwuwu»", async () => {
    // Arrange: доска по тайтлу; счётчик ядра — независимой пробой page 1
    const found = await tool<{ matches?: { id: string; inTrash?: boolean }[] }>(
      "find_doc_by_title", { title: BOARD_TITLE, caseInsensitive: true, limit: 5 })
    const docId = found.matches?.find((m) => m.inTrash !== true)?.id
    expect(docId).toBeTruthy()
    const doc = await tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
    const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id
    expect(dbId).toBeTruthy()
    const probe = await __test.renderBoardPage(dbId!, 1)
    const rowCount = probe.view?.rowCount ?? 0
    expect(rowCount).toBeGreaterThan(0)

    // Act
    const r = await tool<{ rows?: unknown[] }>("read_database_cells", { databaseBlockId: dbId })

    // Assert: полный состав (до фикса: 50 из 145 — красный факт одной пробой)
    expect((r.rows ?? []).length).toBeGreaterThanOrEqual(rowCount)
  })
})
