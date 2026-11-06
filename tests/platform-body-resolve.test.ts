// tests/platform-body-resolve.test.ts — контракт резолва док-тела (баг
// «тело рендерится в unsupported — агент с пустым промптом», задача
// 20261104072043): read_database_cells несёт привязку строки к док-телу
// (value.block.id; value.blockID — это id строки), doc_exists различает
// подтверждённо отсутствующий док (isError «was not found») от сбоя ядра
// (throw наверх = infra), doc_ids_by_hpath — FS-проба файлового дерева,
// get_doc отдаёт box/hpath для title-fallback. Живое ядро не нужно —
// fetch скриптуется (паттерн platform-mcp-errors.test.ts).
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import * as mcp from "../lib/platform"

const originalFetch = globalThis.fetch

const AV_ID = "20260101000000-abcdef"

interface McpToolCall {
  name: string
  args: Record<string, unknown>
}

interface RestCall {
  path: string
  body: Record<string, unknown>
}

/** Скриптованный fetch: /api/query/sql — по stmt-скрипту, /mcp — по
 *  tools/call-скрипту (handshake + один resonder), REST-лега ядра — по
 *  path-скрипту. Все вызовы фиксируются. */
function scriptKernel(
  sqlBy: (stmt: string) => unknown[] | undefined,
  toolBy: (call: McpToolCall) => Record<string, unknown>,
  restBy: (call: RestCall) => unknown,
): { toolCalls: McpToolCall[]; restCalls: RestCall[] } {
  const toolCalls: McpToolCall[] = []
  const restCalls: RestCall[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
    if (url.includes("/api/query/sql")) {
      const stmt = String(body.stmt ?? "")
      return new Response(JSON.stringify({ code: 0, msg: "", data: sqlBy(stmt) ?? [] }), { status: 200 })
    }
    if (url.includes("/api/filetree/getIDsByHPath")) {
      restCalls.push({ path: url, body })
      return new Response(JSON.stringify({ code: 0, msg: "", data: restBy({ path: url, body }) }), { status: 200 })
    }
    if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url}`)
    if (body.method === "initialize") {
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "s1" } })
    }
    if (body.method === "notifications/initialized") return new Response("", { status: 200 })
    if (body.method === "tools/call") {
      const params = body.params as { name?: string; arguments?: Record<string, unknown> }
      const call = { name: params.name ?? "?", args: params.arguments ?? {} }
      toolCalls.push(call)
      const payload = JSON.stringify({ jsonrpc: "2.0", id: 1, result: toolBy(call) })
      return new Response(`event: message\ndata: ${payload}\n\n`, { status: 200 })
    }
    throw new Error(`mock: неожиданный mcp-метод ${String(body.method)}`)
  }) as typeof fetch
  return { toolCalls, restCalls }
}

/** SQL-скрипт по умолчанию: avIdOfBlock по databaseBlockId. */
const sqlAvOnly = (stmt: string): unknown[] | undefined => {
  if (stmt.includes("select markdown from blocks")) {
    return [{ markdown: `<div data-av-id="${AV_ID}"></div>` }]
  }
  return undefined
}

/** Render-страница: page 1 — строки доски, дальше пусто (терминация выборки). */
const renderWith = (rows: unknown[]): ((call: McpToolCall) => Record<string, unknown>) => {
  let page = 0
  return () => {
    page++
    return page === 1
      ? { structuredContent: { view: { columns: [{ id: TITLE_KEY, name: "Title" }], rows, rowCount: rows.length } } }
      : { structuredContent: { view: { rows: [] } } }
  }
}

const TITLE_KEY = "k-title"

/** Block-клетка Title: bound (id привязанного дока) или detached. */
const blockCell = (content: string, docId?: string) => ({
  value: { keyID: TITLE_KEY, type: "block", block: { content, ...(docId !== undefined ? { id: docId } : {}) } },
})

beforeEach(() => {
  mcp.__test.expireSession()
})

afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("read_database_cells: привязка строки к док-телу", () => {
  test("bound-строка → linkedDocId из block-клетки, detached → null", async () => {
    scriptKernel(
      sqlAvOnly,
      renderWith([
        { id: "rowB", cells: [blockCell("крон с телом", "20261105122409-4bekccw")] },
        { id: "rowD", cells: [blockCell("detached-строка")] },
      ]),
      () => ({}),
    )
    const r = (await mcp.tool<{ rows?: { linkedDocId?: string | null; title?: string }[] }>(
      "read_database_cells", { docId: "doc1", databaseBlockId: "av1" }))
    expect(r.rows?.map((row) => row.linkedDocId)).toEqual(["20261105122409-4bekccw", null])
    expect(r.rows?.map((row) => row.title)).toEqual(["крон с телом", "detached-строка"])
  })
})

describe("doc_exists: вердикт «дока нет» vs сбой ядра", () => {
  test("живой док — текст block get несёт Type: NodeDocument", async () => {
    scriptKernel(sqlAvOnly, (call) => {
      if (call.name !== "block") throw new Error(`mock: неожиданная тулза ${call.name}`)
      return { content: [{ type: "text", text: "ID: 20261105122409-4bekccw\nType: NodeDocument\nHPath: /x" }] }
    }, () => ({}))
    const r = await mcp.tool<{ exists?: boolean }>("doc_exists", { docId: "20261105122409-4bekccw" })
    expect(r.exists).toBe(true)
  })

  test("подтверждённо отсутствующий (isError «was not found») → exists:false, не throw", async () => {
    scriptKernel(sqlAvOnly, () => ({
      isError: true,
      content: [{ type: "text", text: "block 2Kr2JN8dDf was not found in a normal notebook; provide notebook for encrypted targets" }],
    }), () => ({}))
    const r = await mcp.tool<{ exists?: boolean }>("doc_exists", { docId: "2Kr2JN8dDf" })
    expect(r.exists).toBe(false)
  })

  test("иной isError — throw наверх (инфра-сбой, не вердикт)", async () => {
    scriptKernel(sqlAvOnly, () => ({ isError: true, content: [{ type: "text", text: "database is syncing" }] }), () => ({}))
    const err = await mcp.tool("doc_exists", { docId: "x" }).then(() => null, (e: Error) => e)
    expect(err).not.toBeNull()
    expect(err?.message).toContain("tool block")
  })
})

describe("doc_ids_by_hpath: FS-проба файлового дерева", () => {
  test("notebook/path проходят насквозь, ids возвращаются", async () => {
    const { restCalls } = scriptKernel(sqlAvOnly, () => { throw new Error("mcp не нужен") }, (call) => {
      expect(call.body.notebook).toBe("20261103224312-fbrf5fp")
      expect(call.body.path).toBe("/uwuwu/Кроны uwuwu/stable-ватчер")
      return ["20261105122409-4bekccw"]
    })
    const r = await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", {
      notebook: "20261103224312-fbrf5fp", path: "/uwuwu/Кроны uwuwu/stable-ватчер",
    })
    expect(r.ids).toEqual(["20261105122409-4bekccw"])
    expect(restCalls.length).toBe(1)
  })

  test("не-массив в ответе ядра → пустые ids, не throw", async () => {
    scriptKernel(sqlAvOnly, () => { throw new Error("mcp не нужен") }, () => null)
    const r = await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", { notebook: "b", path: "/p" })
    expect(r.ids).toEqual([])
  })
})

describe("get_doc: box и hpath для title-fallback", () => {
  test("селект несёт box/hpath, тулза их возвращает", async () => {
    const stmts: string[] = []
    scriptKernel((stmt) => {
      stmts.push(stmt)
      if (stmt.includes("box, hpath from blocks")) {
        return [{ id: "doc9", content: "Кроны uwuwu", updated: "20261105000000", box: "20261103224312-fbrf5fp", hpath: "/uwuwu/Кроны uwuwu" }]
      }
      return undefined
    }, () => { throw new Error("mcp не нужен") }, () => ({}))
    const r = await mcp.tool<{ box?: string; hpath?: string }>("get_doc", { docId: "doc9" })
    expect(r.box).toBe("20261103224312-fbrf5fp")
    expect(r.hpath).toBe("/uwuwu/Кроны uwuwu")
    expect(stmts.some((s) => s.includes("box, hpath"))).toBe(true)
  })
})
