// tests/cron-infra-tick.test.ts — контракт тика на инфра-сбое ядра (ревью
// раунда 1, Common-2): resolveBody → infra → тик НЕ пишет state, НЕ трогает
// доску (Last Run/Status → one-shot жив), НЕ спавнит сессию. Сценарий —
// SQL-лаг индекса доски (get_doc без box/hpath, детач-строка): быстрый
// infra-путь без ретраев адаптера. HOME подменяется до импорта демона —
// лог/state изолированы; fetch скриптуется на все транспортные пути.
import { mkdirSync } from "node:fs"
import { afterEach, beforeEach, describe, expect, test } from "bun:test"

const TEST_HOME = "/tmp/opencode/cron-tick-home"
process.env.HOME = TEST_HOME
mkdirSync(TEST_HOME, { recursive: true })

const cron = await import("../cron/cron")
const mcp = await import("../lib/platform")
const originalFetch = globalThis.fetch

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const SCHED_KEY = "k-sched"
const STATUS_KEY = "k-status"
const RUN_KEY = "k-run"

interface KernelCall {
  kind: "mcp-tool" | "sql"
  name: string
}

function scriptKernel(): { calls: KernelCall[] } {
  const calls: KernelCall[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
    if (url.includes("/api/query/sql")) {
      const stmt = String(body.stmt ?? "")
      calls.push({ kind: "sql", name: stmt })
      // доска не в индексе (лаг SQL) — get_doc молча без box/hpath → infra
      if (stmt.includes("box, hpath from blocks")) return json({ code: 0, msg: "", data: [] })
      if (stmt.includes("select markdown from blocks")) {
        return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] })
      }
      return json({ code: 0, msg: "", data: [] })
    }
    if (url.includes("/api/export/exportMdContent") || url.includes("/api/block/appendBlock") || url.includes("/api/filetree/")) {
      throw new Error(`mock: неожиданная REST-запись/чтение ${url}`)
    }
    if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url} (opencode REST не должен зваться)`)
    if (body.method === "initialize") return sse({}, "s1")
    if (body.method === "notifications/initialized") return new Response("", { status: 200 })
    if (body.method === "tools/call") {
      const params = body.params as { name?: string; arguments?: { action?: string; page?: number } }
      calls.push({ kind: "mcp-tool", name: `${params.name}:${params.arguments?.action ?? "?"}` })
      if (params.name === "database" && params.arguments?.action === "render") {
        // страница 1 каждого прохода (начальный срез + гейт) — со строками
        if (params.arguments.page === 1) {
          return sse({ structuredContent: { view: {
            columns: [
              { id: TITLE_KEY, name: "Title" }, { id: SCHED_KEY, name: "Schedule" },
              { id: STATUS_KEY, name: "Status" }, { id: RUN_KEY, name: "Last Run" },
            ],
            rows: [{
              id: "20261105210000-row0001",
              cells: [
                // detached-строка (без block.id): резолв сразу в title-fallback,
                // где get_doc упирается в лаг индекса → infra без медленных ретраев
                { value: { keyID: TITLE_KEY, type: "block", block: { content: "крон X" } } },
                { value: { keyID: SCHED_KEY, type: "text", text: { content: "09:30" } } },
                { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "active" }] } },
                { value: { keyID: RUN_KEY, type: "text", text: { content: "" } } },
              ],
            }],
            rowCount: 1,
          } } })
        }
        return sse({ structuredContent: { view: { rows: [] } } })
      }
      throw new Error(`mock: неожиданная mcp-тулза ${params.name}`)
    }
    throw new Error(`mock: неожиданный mcp-метод ${String(body.method)}`)
  }) as typeof fetch
  return { calls }
}

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })
const sse = (result: unknown, sessionId?: string): Response =>
  new Response(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: 1, result })}\n\n`,
    { status: 200, headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {} })

beforeEach(() => {
  mcp.__test.expireSession()
})

afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("tick: инфра-сбой ядра ≠ вердикт — слот крона не сжигается", () => {
  test("SQL-лаг доски → infra: state пуст, доска не пишется, сессии нет", async () => {
    const { calls } = scriptKernel()
    const state = { crons: {} as Record<string, unknown> }
    // 09:35 при расписании 09:30 — due (догон); one-shot: гасился бы в fired
    // при refuse, но infra не имеет права тронуть ни state, ни доску
    await cron.tickBoard(state, { docId: "20261105210000-board1", dbId: "20261105210000-avblock1" }, "Кроны uwuwu", new Date("2026-11-05T09:35:00"))
    expect(Object.keys(state.crons)).toEqual([])
    const writes = calls.filter((c) => c.name.includes("item_update"))
    expect(writes).toEqual([])
    // тик дошёл до резолва (get_doc доски) и до обоих срезов (начальный + гейт)
    expect(calls.some((c) => c.kind === "sql" && c.name.includes("box, hpath from blocks"))).toBe(true)
    expect(calls.filter((c) => c.name === "database:render").length).toBeGreaterThanOrEqual(2)
  })
})
