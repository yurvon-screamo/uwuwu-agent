// tests/cron-infra-tick.test.ts — контракт тика на инфра-сбое ядра (ревью
// раунда 1, Common-2): resolveBody → infra → тик НЕ пишет state, НЕ спавнит
// сессию и не создаёт ран-карточку. Сценарий: экспорт тела мастера отвечает
// отказом ядра (exists:false при живой привязке) — вердикт откладывается до
// подтверждения doc_exists (block get), который подтверждает живость →
// infra-путь (ретрай следующим тиком, семантика классического cron).
// Лог демона изолируется явным env UWUWU_CRON_LOG ДО импорта: os.homedir()
// в Bun не следует за runtime-подменой process.env.HOME — HOME-изоляция
// каталога лога не работает (ревью раунда 2). fetch скриптуется на все
// транспортные пути.
import { mkdirSync } from "node:fs"
import { dirname } from "node:path"
import { afterEach, beforeEach, describe, expect, test } from "bun:test"

const TEST_LOG = "/tmp/opencode/cron-tick-home/cron-test.log"
process.env.UWUWU_CRON_LOG = TEST_LOG
mkdirSync(dirname(TEST_LOG), { recursive: true })

const cron = await import("../cron/cron")
const mcp = await import("../lib/platform")
const originalFetch = globalThis.fetch
const originalSpawn = Bun.spawn

/** CLI-спавны скриптуются: дефолт CLI_BIN резолвится в ~/.cargo/bin/uwuwu-cli,
 *  которого на CI-раннере нет — Bun.spawn кинул бы ENOENT. argv фиксируется:
 *  reconcile клеток мастера честно спавнит task edit (ассерт в тесте). */
const spawnArgv: string[][] = []
function scriptSpawn(): void {
  spawnArgv.length = 0
  ;(Bun as unknown as { spawn: unknown }).spawn = ((argv: string[]) => {
    spawnArgv.push(argv.map(String))
    return {
      stdout: new Response("").body,
      stderr: new Response("").body,
      exited: Promise.resolve(0),
    } as unknown as ReturnType<typeof Bun.spawn>
  }) as typeof Bun.spawn
}

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261107000000-board1"
const AV_BLOCK = "20261107000000-avblk1"
const ROW_ID = "20261107000000-row0001"
const BODY_DOC = "20261107000000-body001"

interface KernelCall {
  kind: "mcp-tool" | "rest"
  name: string
}

function scriptKernel(): { calls: KernelCall[] } {
  const calls: KernelCall[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
    if (url.includes("/api/attr/getBlockAttrs")) {
      calls.push({ kind: "rest", name: "getBlockAttrs" })
      return json({ code: 0, msg: "", data: { "custom-schedule": "5m" } })
    }
    if (url.includes("/api/export/exportMdContent")) {
      calls.push({ kind: "rest", name: "exportMdContent" })
      // отказ ядра на экспорте: exists:false — distinguishable from infra
      // только пробой block get (см. resolveBody)
      return json({ code: -1, msg: "kernel boom" })
    }
    if (url.includes("/api/query/sql")) {
      const stmt = String(body.stmt ?? "")
      calls.push({ kind: "rest", name: stmt.slice(0, 40) })
      if (stmt.includes("select markdown from blocks")) {
        return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] })
      }
      return json({ code: 0, msg: "", data: [] })
    }
    if (url.includes("/api/block/appendBlock") || url.includes("/api/filetree/")) {
      throw new Error(`mock: неожиданная REST-запись/чтение ${url}`)
    }
    if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url} (opencode REST не должен зваться)`)
    if (body.method === "initialize") return sse({}, "s1")
    if (body.method === "notifications/initialized") return new Response("", { status: 200 })
    if (body.method === "tools/call") {
      const params = body.params as { name?: string; arguments?: { action?: string; page?: number } }
      calls.push({ kind: "mcp-tool", name: `${params.name}:${params.arguments?.action ?? "?"}` })
      if (params.name === "database" && params.arguments?.action === "render") {
        if (params.arguments.page === 1) {
          return sse({ structuredContent: { view: {
            columns: [
              { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
            ],
            rows: [{
              id: ROW_ID,
              cells: [
                { value: { keyID: TITLE_KEY, type: "block", block: { content: "мастер X", refSubtype: "d", id: BODY_DOC } } },
                { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "Вейт" }] } },
              ],
            }],
            rowCount: 1,
          } } })
        }
        return sse({ structuredContent: { view: { columns: [], rows: [], rowCount: 0 } } })
      }
      if (params.name === "block" && params.arguments?.action === "get") {
        // FS-истина: блок жив — отказ экспорта не превращается в вердикт
        // «тела нет», это infra (ретрай следующим тиком)
        calls.push({ kind: "mcp-tool", name: "block:get-alive" })
        return sse({ content: [{ type: "text", text: `Markdown: x\nId: ${BODY_DOC}\nType: NodeDocument` }] })
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
  scriptSpawn()
})

afterEach(() => {
  globalThis.fetch = originalFetch
  ;(Bun as unknown as { spawn: unknown }).spawn = originalSpawn
})

describe("tick: инфра-сбой ядра ≠ вердикт — слот мастера не сжигается", () => {
  // Отказ экспорта ретраится адаптером (4 попытки с паузами) — окно > дефолта
  test("экспорт тела падает при живом блоке → infra: state пуст, ран не создаётся", async () => {
    const { calls } = scriptKernel()
    const state = { crons: {} }
    await cron.tickBoard(state, { docId: BOARD_DOC, dbId: AV_BLOCK }, "Задачи uwuwu", new Date("2026-11-07T09:35:00"))
    expect(Object.keys(state.crons)).toEqual([])
    // тик дошёл до атрибутов и экспорта, отличил infra от отсутствия тела
    // пробой block get
    expect(calls.some((c) => c.name === "getBlockAttrs")).toBe(true)
    expect(calls.some((c) => c.name === "exportMdContent")).toBe(true)
    expect(calls.some((c) => c.name === "block:get-alive")).toBe(true)
    // гейт-срез состоял (начальный + гейт)
    expect(calls.filter((c) => c.name === "database:render").length).toBeGreaterThanOrEqual(2)
    // ран-карточка и сессия не создавались — оба пути письма:
    // MCP-тулзы (create_comment/item_update) и CLI-спавны
    expect(calls.filter((c) => c.kind === "mcp-tool" && (c.name.includes("create_comment") || c.name.includes("item_update")))).toEqual([])
    expect(spawnArgv.some((a) => a[1] === "task" && a[2] === "create")).toBe(false)
    // CLI-спавны есть (reconcile клеток мастера) — и все это task edit
    expect(spawnArgv.length).toBeGreaterThan(0)
    expect(spawnArgv.every((a) => a[1] === "task" && a[2] === "edit")).toBe(true)
  }, 20_000)
})
