// tests/cron-dryrun-child.ts — дочерний процесс для cron-dryrun-refuse.test.ts:
// DRY_RUN — константа модуля демона (печётся на импорте), в общем процессе
// bun test её не подменить без утечки в соседние тест-файлы. Env задаётся
// здесь до импорта cron.ts — паттерн single-instance-child.ts.
// ОБА sandbox-env обязательны ДО импорта: прод-кейс 06.11 (ревью раунд 4,
// High) — child без UWUWU_CRON_STATE_DIR писал refuse-запись в прод-state
// демона (saveState в refuse-пути). Теперь инвариант «DRY_RUN ⇒ ни одной
// записи» держится гардом saveState (cron.ts) — env остаётся страховкой
// на случай отката гарда: песочница изолирует state+lock независимо.
import { mkdirSync } from "node:fs"

process.env.UWUWU_CRON_LOG = "/tmp/opencode/cron-dryrun-home/cron-dryrun-child.log"
process.env.UWUWU_CRON_STATE_DIR = "/tmp/opencode/cron-dryrun-home/state"
process.env.UWUWU_CRON_DRY_RUN = "1"
mkdirSync("/tmp/opencode/cron-dryrun-home/state", { recursive: true })

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const SCHED_KEY = "k-sched"
const STATUS_KEY = "k-status"
const RUN_KEY = "k-run"
const BOARD_DOC = "20261106000000-board1"
const ROW_ID = "20261106000000-row0001"

interface Call {
  kind: "mcp-tool" | "rest"
  name: string
}
const calls: Call[] = []

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
  if (url.includes("/api/filetree/getIDsByHPath")) {
    calls.push({ kind: "rest", name: "getIDsByHPath" })
    // саб-дока по заголовку нет → refuse «строка без тела»
    return json({ code: 0, msg: "", data: [] })
  }
  if (url.includes("/api/query/sql")) {
    const stmt = String(body.stmt ?? "")
    if (stmt.includes("box, hpath from blocks")) {
      return json({ code: 0, msg: "", data: [{ id: BOARD_DOC, content: "Кроны uwuwu", updated: "20261106000000", box: "20260101000000-box", hpath: "/Projects/uwuwu/Кроны uwuwu" }] })
    }
    if (stmt.includes("select markdown from blocks")) {
      return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] })
    }
    return json({ code: 0, msg: "", data: [] })
  }
  if (url.includes("/api/export/exportMdContent") || url.includes("/api/block/appendBlock")) {
    throw new Error(`mock: неожиданная REST-запись/чтение ${url}`)
  }
  if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url}`)
  if (body.method === "initialize") return sse({}, "s1")
  if (body.method === "notifications/initialized") return new Response("", { status: 200 })
  if (body.method === "tools/call") {
    const params = body.params as { name?: string; arguments?: { action?: string; page?: number } }
    calls.push({ kind: "mcp-tool", name: `${params.name}:${params.arguments?.action ?? "?"}` })
    if (params.name === "database" && params.arguments?.action === "render") {
      if (params.arguments.page === 1) {
        return sse({ structuredContent: { view: {
          columns: [
            { id: TITLE_KEY, name: "Title" }, { id: SCHED_KEY, name: "Schedule" },
            { id: STATUS_KEY, name: "Status" }, { id: RUN_KEY, name: "Last Run" },
          ],
          rows: [{
            id: ROW_ID,
            cells: [
              // detached-строка: резолв уходит в title-fallback → 0 доков → refuse
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

function json(data: unknown): Response {
  return new Response(JSON.stringify(data), { status: 200 })
}
function sse(result: unknown, sessionId?: string): Response {
  return new Response(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: 1, result })}\n\n`,
    { status: 200, headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {} })
}

// литерал без каста: tickBoard принимает CronState (Record<string, TrackedCron>)
const state = { crons: {} }
try {
  await cron.tickBoard(state, { docId: BOARD_DOC, dbId: "20261106000000-avblock1" }, "Кроны uwuwu", new Date("2026-11-06T09:35:00"))
} catch (e) {
  console.log(JSON.stringify({ error: String(e), calls }))
  process.exit(0)
}
console.log(JSON.stringify({ state: state.crons, calls }))
