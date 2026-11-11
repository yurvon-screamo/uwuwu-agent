// tests/platform-pagination.test.ts — контракт полноты выборки доски.
// Серверный pageSize database render — 50: один вызов отдаёт только первую
// страницу (живая проба 05.11: rowCount=145, в ответе ровно 50) — адаптер
// обязан пагинировать. Юниты цикла — на фейковых страницах; живой тест —
// против реальной доски «Задачи uwuwu» с независимой пробой rowCount.
import { afterEach, describe, expect, test } from "bun:test"
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

describe("read_database_cells: полнота доски (живое ядро)", () => {
  const hasEnv = (process.env.UWUWU_SIYUAN_TOKEN ?? process.env.UWUWU_SIYUAN_WS ?? "") !== ""

  // Живой тест против растущей доски: read_database_cells сам по себе
  // ~5s при 300+ строках (пагинация по 50) — дефолтный таймаут bun 5s
  // ловил ложный красный, бюджет 30s.
  test.skipIf(!hasEnv)("адаптер видит ≥ rowCount строк доски «Задачи uwuwu»", async () => {
    // Arrange: доска по тайтлу; счётчик ядра — независимой пробой page 1
    const found = await tool<{ matches?: { id: string; inTrash?: boolean }[] }>(
      "find_doc_by_title", { title: BOARD_TITLE, caseInsensitive: true, limit: 5 })
    const docId = found.matches?.find((m) => m.inTrash !== true)?.id
    expect(docId).toBeTruthy()
    const doc = await tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
    const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id ?? ""
    expect(dbId).not.toBe("")
    const probe = await __test.renderBoardPage(dbId, 1)
    const rowCount = probe.view?.rowCount ?? 0
    expect(rowCount).toBeGreaterThan(0)

    // Act
    const r = await tool<{ rows?: unknown[] }>("read_database_cells", { databaseBlockId: dbId })

    // Assert: полный состав (до фикса: 50 из 145 — красный факт одной пробой)
    expect((r.rows ?? []).length).toBeGreaterThanOrEqual(rowCount)
  }, 30_000)
})


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

describe("read_database_cells: связка строка→тело (linkedDocId)", () => {
  const originalFetch = globalThis.fetch

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  /** Строка доски: block-ячейка Primary Key (линк на док-тела) + Status.
   *  Шейп value — канонический render живого ядра: blockID = id строки,
   *  block.id = docId тела (≠ blockID), isDetached у detached-строк. */
  const rowOf = (id: string, block: { id?: string; content: string; refSubtype?: string }, isDetached = false) => ({
    id,
    cells: [
      { value: { keyID: "k-key", type: "block", blockID: id, isDetached: isDetached || undefined, block } },
      { value: { keyID: "k-status", type: "select", mSelect: [{ content: "Design" }] } },
    ],
  })

  function scriptBoard(rows: unknown[]): void {
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url.includes("/api/query/sql")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: '<div data-av-id="20260101000000-abcdef"></div>' }] }), { status: 200 })
      }
      if (url.includes("/mcp")) {
        const payload = JSON.parse(String(init?.body ?? "{}")) as { arguments?: { page?: number } }
        const page = payload.arguments?.page ?? 1
        const result = page === 1
          ? { structuredContent: { data: { view: { columns: [{ id: "k-status", name: "Status" }], rowCount: rows.length, rows } } } }
          : { structuredContent: { data: { view: { rows: [] } } } }
        return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), { status: 200 })
      }
      throw new Error(`mock: неожиданный путь ${url}`)
    }) as typeof fetch
  }

  test("block-ячейка с refSubtype=d даёт doc-id тела, не null", async () => {
    // Arrange: до фикса linkedDocId был хардкодом null — wakeup-контур
    // фабрики (комменты/правки тела) не находил тело задачи.
    // Фикстура — живой render ядра (доска 20261104000238-96ch3l2, стр. 1):
    // block.id = docId тела, blockID = id строки, refSubtype "d"
    scriptBoard([rowOf("row1", { id: "20261105000000-bodydoc", content: "Задача один", refSubtype: "d" })])

    // Act
    const r = await tool<{ rows?: { linkedDocId?: string | null; title?: string; cells?: Record<string, { value?: string }> }[] }>(
      "read_database_cells", { docId: "doc1", databaseBlockId: "av1" })

    // Assert
    expect(r.rows?.[0]?.linkedDocId).toBe("20261105000000-bodydoc")
    expect(r.rows?.[0]?.title).toBe("Задача один")
    expect(r.rows?.[0]?.cells?.Status?.value).toBe("Design")
  })

  test("detached-ячейка без block.id — linkedDocId null", async () => {
    // Arrange: detached-строка живого ядра (20261104092509-xo3dxzn) несёт
    // block = {content, created, updated} — поля id нет вовсе
    scriptBoard([rowOf("row2", { content: "просто текст" }, true)])

    // Act
    const r = await tool<{ rows?: { linkedDocId?: string | null }[] }>(
      "read_database_cells", { docId: "doc1", databaseBlockId: "av1" })

    // Assert
    expect(r.rows?.[0]?.linkedDocId).toBeNull()
  })

  test("bound-легаси без refSubtype — docId не теряется", async () => {
    // Arrange: контракт CLI parse_render_row — value.block.id извлекается
    // безусловно; guard на refSubtype терял бы строки, которые ядро ещё
    // не прогнало через NormalizeBlockRefSubtype (легаси-данные)
    scriptBoard([rowOf("row3", { id: "20261105000000-legacydoc", content: "Легаси-строка" })])

    // Act
    const r = await tool<{ rows?: { linkedDocId?: string | null }[] }>(
      "read_database_cells", { docId: "doc1", databaseBlockId: "av1" })

    // Assert
    expect(r.rows?.[0]?.linkedDocId).toBe("20261105000000-legacydoc")
  })
})
