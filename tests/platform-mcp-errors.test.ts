// tests/platform-mcp-errors.test.ts — контракт ошибок mcpRpc (баг «JSON Parse
// error маскирует причину», задача 20261105140450): не-JSON/конверт ядра дают
// честную ошибку со статусом и сниппетом; 404 при отправленной сессии лечится
// одним re-handshake, без цикла. Живое ядро не нужно — fetch скриптуется.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import * as mcp from "../lib/platform"

const originalFetch = globalThis.fetch

interface McpCall {
  method: string
  hasSession: boolean
}

const AV_ID = "20260101000000-abcdef"

/** Скриптованный fetch: REST-лега ядра (/api/query/sql) отвечает кодом 0,
 *  лега /mcp — по скрипту; mcp-вызовы фиксируются (method/hasSession). */
function scriptMcp(respond: (call: number, c: McpCall) => Response): { calls: McpCall[] } {
  const calls: McpCall[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes("/api/query/sql")) {
      // markdown с data-av-id — контракт avIdOfBlock (REST-лега read_database_cells)
      return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] }), { status: 200 })
    }
    if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url}`)
    const payload = JSON.parse(String(init?.body ?? "{}")) as { method?: string }
    const headers = (init?.headers ?? {}) as Record<string, string>
    const hasSession = Object.keys(headers).some((h) => h.toLowerCase() === "mcp-session-id")
    const call = calls.length
    calls.push({ method: payload.method ?? "?", hasSession })
    return respond(call, { method: payload.method ?? "?", hasSession })
  }) as typeof fetch
  return { calls }
}

const jsonrpc = (result: unknown, sessionId?: string): Response =>
  new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), {
    status: 200,
    headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {},
  })

const callPage = (rows: unknown[]): Response =>
  jsonrpc({ structuredContent: { view: { rows } } })

const sse = (result: unknown): Response => {
  const payload = JSON.stringify({ jsonrpc: "2.0", id: 1, result })
  return new Response(`event: message\ndata: ${payload}\n\n`, { status: 200 })
}

/** Скрипт render-страниц (контракт пагинации ядра: page 1 несёт rowCount,
 *  дальше пустая страница терминирует выборку адаптера); формат ответа
 *  задаёт wrap (jsonrpc / sse). mcpRpc-тестам не нужна полнота доски —
 *  одна строка на page 1 достаточно. */
const pagedRender = (wrap: (result: unknown) => Response): ((call: number) => Response) => {
  let page = 0
  return (_call) => {
    page++
    return page === 1
      ? wrap({ structuredContent: { view: { rows: [{ id: "rowX", cells: [] }], rowCount: 1 } } })
      : wrap({ structuredContent: { view: { rows: [] } } })
  }
}

/** Handshake mcp-сессии: initialize → initialized; tools/call — по скрипту. */
const handshake = (sessionId: string, toolResponder: (call: number, c: McpCall) => Response) =>
  (call: number, c: McpCall): Response => {
    if (c.method === "initialize") return jsonrpc({}, sessionId)
    if (c.method === "notifications/initialized") return new Response("", { status: 200 })
    return toolResponder(call, c)
  }

const readCells = (): Promise<unknown> =>
  mcp.tool("read_database_cells", { docId: "doc1", databaseBlockId: "av1" })

beforeEach(() => {
  mcp.__test.expireSession()
})

afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("mcpRpc: протухшая сессия (рестарт ядра)", () => {
  test("404 при сессии в заголовке — один re-handshake, вызов проходит", async () => {
    // первый tools/call с сессией отклоняется 404 → reconnect → успех
    const render = pagedRender(jsonrpc)
    const { calls } = scriptMcp(handshake("s1", (call) => (call === 2 ? new Response("session not found", { status: 404 }) : render(call))))
    const r = (await readCells()) as { rows?: { rowBlockId?: string }[] }
    expect(r.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
    expect(calls.map((c) => c.method)).toEqual(["initialize", "notifications/initialized", "tools/call", "initialize", "notifications/initialized", "tools/call"])
    expect(calls[3]?.hasSession).toBe(false) // reconnect идёт с чистой сессией
  })

  test("второй 404 подряд — описательная ошибка, без цикла", async () => {
    // каждый tools/call с сессией отклоняется; reconnect выполняется один раз
    const { calls } = scriptMcp(handshake("s1", () => new Response("session not found", { status: 404 })))
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err).not.toBeNull()
    expect(err?.message).toContain("HTTP 404")
    expect(err?.constructor.name).not.toBe("SyntaxError")
    expect(calls.filter((c) => c.method === "initialize").length).toBe(2) // ровно один reconnect
  })

  test("404 без сессии в заголовке — ошибка без ретрая", async () => {
    // initialize отвечает без mcp-session-id → tools/call уходит без заголовка
    const { calls } = scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({})
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response("session not found", { status: 404 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("HTTP 404")
    expect(calls.map((c) => c.method)).toEqual(["initialize", "notifications/initialized", "tools/call"])
  })
})

describe("mcpRpc: честные ошибки вместо молчаливых пустых результатов", () => {
  test("kernel-конверт 401 «Auth failed» — throw, не пустой {}", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response(JSON.stringify({ code: -1, msg: "Auth failed [session]" }), { status: 401 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("kernel code=-1")
    expect(err?.message).toContain("Auth failed")
  })

  test("не-JSON тело при 200 (мусор шлюза) — ошибка со сниппетом", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response("<html>gateway oops</html>", { status: 200 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("не-JSON")
    expect(err?.message).toContain("gateway oops")
  })

  test("jsonrpc error envelope — как раньше, с текстом ошибки", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, error: { code: 0, message: "method invalid" } }), { status: 200 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("method invalid")
  })

  test("успешный вызов в SSE-обёртке (data:-строки) — парсится как раньше", async () => {
    const render = pagedRender(sse)
    scriptMcp(handshake("s1", (_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return render(0)
    }))
    const r = (await readCells()) as { rows?: { rowBlockId?: string }[] }
    expect(r.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
  })
})

