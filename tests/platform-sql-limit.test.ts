// tests/platform-sql-limit.test.ts — контракт границ SQL против дефолт-лимита ядра.
// Ядро SiYuan /api/query/sql без явного LIMIT молча отдаёт первые 64 строки
// (подтверждено на живом ядре 04.11) — каждый stmt адаптера обязан нести limit.
import { afterEach, describe, expect, test } from "bun:test"
import { tool } from "../lib/platform"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function captureSqlStmts(): string[] {
  const stmts: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes("/api/query/sql")) {
      const body = JSON.parse(String(init?.body ?? "{}")) as { stmt?: string }
      stmts.push(String(body.stmt ?? ""))
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }),
        { headers: { "Content-Type": "application/json" } })
    }
    throw new Error(`mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return stmts
}

describe("sql границы (kernel default LIMIT 64)", () => {
  test("read_doc несёт явный limit в stmt", async () => {
    const stmts = captureSqlStmts()
    await tool("read_doc", { docId: "20261101000000-deadbeef" })
    expect(stmts).toHaveLength(1)
    expect(stmts[0]).toMatch(/limit\s+\d+/i)
  })

  test("list_organize_nodes несёт явный limit в stmt", async () => {
    const stmts = captureSqlStmts()
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url.includes("/api/notebook/lsNotebooks")) {
        return new Response(JSON.stringify({
          code: 0, msg: "", data: { notebooks: [{ id: "nb1", name: "Projects", closed: false }] },
        }), { headers: { "Content-Type": "application/json" } })
      }
      if (url.includes("/api/query/sql")) {
        const body = JSON.parse(String(init?.body ?? "{}")) as { stmt?: string }
        stmts.push(String(body.stmt ?? ""))
        return new Response(JSON.stringify({ code: 0, msg: "", data: [] }),
          { headers: { "Content-Type": "application/json" } })
      }
      throw new Error(`mock: неожиданный путь ${url}`)
    }) as typeof fetch
    await tool("list_organize_nodes", {})
    expect(stmts.length).toBeGreaterThan(0)
    for (const stmt of stmts) expect(stmt).toMatch(/limit\s+\d+/i)
  })
})
