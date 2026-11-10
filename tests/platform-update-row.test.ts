// tests/platform-update-row.test.ts — контракт update_database_row: запись
// только в существующие колонки. Регрессия фикса фоллбэка «колонки нет →
// ключ Status» (миграционный атавизм 6bd9307, вылечен задачей
// 20261104064814): запись «Сессия тест» на доске без этой колонки обязана
// быть true no-op (best-effort контракт setCellsSafe), а не искажением
// Status-ячейки каждой Done-чистки.
import { afterEach, describe, expect, test } from "bun:test"
import { tool } from "../lib/platform"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

interface KeyDef { id: string; name: string }

/** Мок ядра+MCP: sql отдаёт пустой markdown (avId = databaseBlockId),
 *  database keys/item_update — под контролем теста. */
function mockKernel(keys: KeyDef[]): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes("/api/query/sql")) {
      calls.push("sql")
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/mcp")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        method?: string
        params?: { name?: string; arguments?: Record<string, unknown> }
      }
      if (payload.method === "initialize") {
        return new Response(JSON.stringify({ jsonrpc: 2.0, id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "sid" } })
      }
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      const action = String(payload.params?.arguments?.action ?? "")
      if (action === "keys") {
        return new Response(JSON.stringify({
          jsonrpc: 2.0, id: 1,
          result: { structuredContent: { data: { keys } } },
        }), { status: 200 })
      }
      if (action === "item_update") {
        const value = (payload.params?.arguments?.value ?? {}) as {
          text?: { content?: string }
          mSelect?: { content?: string }[]
        }
        const content = value.text?.content ?? value.mSelect?.map((o) => o.content ?? "").join(",") ?? "?"
        calls.push(`item_update:${String(payload.params?.arguments?.keyID)}=${content}`)
        return new Response(JSON.stringify({
          jsonrpc: 2.0, id: 1,
          result: { structuredContent: { data: {} } },
        }), { status: 200 })
      }
      throw new Error(`mock: неожиданный action database ${action}`)
    }
    throw new Error(`mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return calls
}

const row = "20261108000000-row1"
// avKeys кэширует ключи по avId — у каждого кейса свой av-блок, иначе кэш
// первого теста заглушит раскладку колонок следующих
const uniqAv = (n: number): string => `20261108000000-av${n}`

describe("update_database_row: запись только в существующие колонки", () => {
  test("колонки нет — true no-op: ноль item_update, Status не искажается, skipped перечисляет имена", async () => {
    // Arrange: доска без колонки «Сессия тест» (деплой-окно фичи)
    const calls = mockKernel([{ id: "k-status", name: "Status" }])

    // Act: фабричная Done-чистка — пустая тест-ячейка + пустые соседние
    const r = await tool("update_database_row", {
      docId: "doc1", databaseBlockId: uniqAv(1), rowBlockId: row,
      cells: { "Сессия dev": "", "Сессия ревью": "", "Сессия тест": "", Branch: "work/x" },
    })

    // Assert: ни одной записи — фоллбэк на Status не сработал;
    // skipped — имена колонок, ушедших в no-op (флаг фабрики, ADR-054)
    expect(calls).toEqual(["sql"])
    expect(r.skipped).toEqual(["Сессия dev", "Сессия ревью", "Сессия тест", "Branch"])
  })

  test("частичная доска — записывается только существующая колонка, skipped только недостающие", async () => {
    const calls = mockKernel([
      { id: "k-status", name: "Status" },
      { id: "k-dev", name: "Сессия dev" },
    ])

    const r = await tool("update_database_row", {
      docId: "doc1", databaseBlockId: uniqAv(4), rowBlockId: row,
      cells: { "Сессия dev": "ses_x", "Сессия ревью": "ses_r" },
    })

    expect(calls).toEqual(["sql", "item_update:k-dev=ses_x"])
    expect(r.skipped).toEqual(["Сессия ревью"])
  })

  test("колонка есть — запись текстом в свой ключ", async () => {
    const calls = mockKernel([
      { id: "k-status", name: "Status" },
      { id: "k-test", name: "Сессия тест" },
    ])

    await tool("update_database_row", {
      docId: "doc1", databaseBlockId: uniqAv(2), rowBlockId: row,
      cells: { "Сессия тест": "ses_new" },
    })

    expect(calls).toEqual(["sql", "item_update:k-test=ses_new"])
  })

  test("Status на доске с колонкой «Статус» — резолв по русскому имени, mSelect-шейп", async () => {
    const calls = mockKernel([{ id: "k-ru", name: "Статус" }])

    await tool("update_database_row", {
      docId: "doc1", databaseBlockId: uniqAv(3), rowBlockId: row,
      cells: { Status: "Needs attention" },
    })

    expect(calls).toEqual(["sql", "item_update:k-ru=Needs attention"])
  })
})
