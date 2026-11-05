// factory-readtasks-child.ts — спавн-ребёнок теста фоллбека тела по тайтлу
// (detached-строки доски, задача 20261105214942; не запускать руками).
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте — иначе log()/siyuanToken() ходят в прод (инцидент 06.11).
// Ребёнок скриптует fetch (ядро), зовёт readTasks напрямую и печатает
// вердикт JSON в stdout — родитель сверяет резолвы и число вызовов.
import { mkdirSync, readFileSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"

const BOARD_DOC = "boarddoc1"
const BOARD_HPATH = "/uwuwu/Задачи x"
const BOARD_BOX = "nb-projects"

/** Строка доски: detached (привязки нет, только content) или bound (block.id). */
function rowOf(id: string, title: string, boundId?: string): unknown {
  const block: Record<string, unknown> = boundId === undefined
    ? { content: title, created: 20261106, updated: 20261106 }
    : { id: boundId, content: title, refSubtype: "d" }
  return {
    id,
    cells: [{
      value: { keyID: "k-key", type: "block", blockID: id, isDetached: boundId === undefined || undefined, block },
    }],
  }
}

const ROWS = [
  rowOf("row-bound", "Bound задача", "20261106000000-boundbod"),
  rowOf("row-single", "Detached single"),
  rowOf("row-orphan", "Detached orphan"),
  rowOf("row-none", "Detached none"),
  rowOf("row-slash", "Deploy: up/down gateway"),
  rowOf("row-cache-a", "Cache A"),
  rowOf("row-cache-b", "Cache B"),
  rowOf("row-cache-c", "Cache C"),
]

const HPATH_TO_IDS: Record<string, string[]> = {
  [`${BOARD_HPATH}/Detached single`]: ["20261106000000-singlebod"],
  [`${BOARD_HPATH}/Detached orphan`]: ["20261106000000-orpha", "20261106000000-orphb"],
  [`${BOARD_HPATH}/Detached none`]: [],
  [`${BOARD_HPATH}/Deploy: up\u{2215}down gateway`]: ["20261106000000-slashbod"],
  [`${BOARD_HPATH}/Cache A`]: ["20261106000000-cachea"],
  [`${BOARD_HPATH}/Cache B`]: ["20261106000000-cacheb"],
  [`${BOARD_HPATH}/Cache C`]: ["20261106000000-cachec"],
}

function scriptKernel(): { hpathCalls: string[]; boardGetDocCalls: number; misses: string[] } {
  const hpathCalls: string[] = []
  const misses: string[] = []
  let boardGetDocCalls = 0
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes("/api/query/sql")) {
      const stmt = (JSON.parse(String(init?.body ?? "{}")) as { stmt?: string }).stmt ?? ""
      if (stmt.includes("box, hpath")) {
        boardGetDocCalls++
        return new Response(JSON.stringify({
          code: 0, msg: "", data: [{ id: BOARD_DOC, content: "Задачи x", updated: "20261106000000", box: BOARD_BOX, hpath: BOARD_HPATH }],
        }), { status: 200 })
      }
      if (stmt.includes("markdown from blocks where id=")) {
        // avIdOfBlock: av-носитель доски → data-av-id
        return new Response(JSON.stringify({
          code: 0, msg: "", data: [{ markdown: '<div data-av-id="20260101000000-abcdef"></div>' }],
        }), { status: 200 })
      }
      misses.push(`sql:${stmt.slice(0, 60)}`)
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/api/filetree/getIDsByHPath")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { notebook?: string; path?: string }
      hpathCalls.push(payload.path ?? "")
      const ids = HPATH_TO_IDS[payload.path ?? ""]
      if (ids === undefined) misses.push(`hpath:${payload.path}`)
      return new Response(JSON.stringify({ code: 0, msg: "", data: ids ?? [] }), { status: 200 })
    }
    if (url.includes("/mcp")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        method?: string
        params?: { name?: string; arguments?: { page?: number } }
      }
      if (payload.method === "initialize") {
        return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "sid" } })
      }
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      const rows = payload.params?.arguments?.page === 1 ? ROWS : []
      const result = { structuredContent: { data: { view: { columns: [], rowCount: rows.length, rows } } } }
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), { status: 200 })
    }
    misses.push(`url:${url.slice(0, 60)}`)
    return new Response(JSON.stringify({ code: -1, msg: "child mock miss" }), { status: 200 })
  }) as typeof fetch
  return { hpathCalls, get boardGetDocCalls() { return boardGetDocCalls }, misses }
}

// ——— песочница: HOME от родителя; лог читаем оттуда же ———
const logPath = join(homedir(), ".local/share/opencode/log/uwuwu-factory.log")
mkdirSync(dirname(logPath), { recursive: true })

const probe = scriptKernel()
const { readTasks } = await import("../factory/factory")
const rows = await readTasks({ docId: BOARD_DOC, dbId: "avblk1" })

let logText = ""
try { logText = readFileSync(logPath, "utf-8") } catch { /* лога ещё нет — тоже валидно */ }

const bodies: Record<string, string> = {}
for (const r of rows) bodies[r.rowId] = r.bodyDoc
console.log(JSON.stringify({
  bodies,
  hpathCalls: probe.hpathCalls,
  boardGetDocCalls: probe.boardGetDocCalls,
  misses: probe.misses,
  log: logText,
}))
