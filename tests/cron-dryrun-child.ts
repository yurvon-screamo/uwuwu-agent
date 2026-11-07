// tests/cron-dryrun-child.ts — дочерний процесс для cron-dryrun-refuse.test.ts:
// DRY_RUN — константа модуля демона (печётся на импорте), в общем процессе
// bun test её не подменить без утечки в соседние тест-файлы. Env задаётся
// здесь до импорта cron.ts — паттерн single-instance-child.ts.
// ОБА sandbox-env обязательны ДО импорта: прод-кейс 06.11 (ревью раунд 4,
// High) — child без UWUWU_CRON_STATE_DIR писал refuse-запись в прод-state
// демона (saveState в refuse-пути). Теперь инвариант «DRY_RUN ⇒ ни одной
// записи» держится гардом saveState (cron.ts) — env остаётся страховкой
// на случай отката гарда: песочница изолирует state+lock независимо.
//
// Сценарий: мастер крон-задачи (Вейт + custom-schedule в атрибутах тела),
// тело рендерится в пустоту (unsupported-легаси) → refuse. В DRY_RUN:
// коммент отказа и state.json не пишутся, ран-карточка (CLI spawn) и
// сессия не создаются.
import { mkdirSync } from "node:fs"

process.env.UWUWU_CRON_LOG = "/tmp/opencode/cron-dryrun-home/cron-dryrun-child.log"
process.env.UWUWU_CRON_STATE_DIR = "/tmp/opencode/cron-dryrun-home/state"
process.env.UWUWU_CRON_DRY_RUN = "1"
mkdirSync("/tmp/opencode/cron-dryrun-home/state", { recursive: true })

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261107000000-board1"
const AV_BLOCK = "20261107000000-avblk1"
const ROW_ID = "20261107000000-row0001"
const BODY_DOC = "20261107000000-body001"

interface Call {
  kind: "mcp-tool" | "rest" | "spawn"
  name: string
}
const calls: Call[] = []

// CLI не зовётся в DRY_RUN: ран-карточка не создаётся. Патчим BEFORE tick:
// реальный spawn писал бы в прод-воркспейс.
const origSpawn = Bun.spawn
;(Bun as unknown as { spawn: unknown }).spawn = ((cmd: unknown, _opts?: unknown) => {
  calls.push({ kind: "spawn", name: String(Array.isArray(cmd) ? cmd[0] : cmd) })
  throw new Error(`mock: DRY_RUN не должен спавнить процессы (${String(cmd)})`)
}) as typeof Bun.spawn

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
  if (url.includes("/api/attr/getBlockAttrs")) {
    calls.push({ kind: "rest", name: "getBlockAttrs" })
    return json({ code: 0, msg: "", data: { "custom-schedule": "*/5 * * * *", title: "мастер" } })
  }
  if (url.includes("/api/export/exportMdContent")) {
    calls.push({ kind: "rest", name: "exportMdContent" })
    // тело из unsupported-комментариев: renderVerdict → refuse
    return json({ code: 0, msg: "", data: { content: "---\ntitle: мастер\n---\n\n# мастер\n\n<!-- unsupported: legacy -->\n" } })
  }
  if (url.includes("/api/block/appendBlock")) {
    calls.push({ kind: "rest", name: "appendBlock" })
    throw new Error("mock: DRY_RUN не пишет в доки (коммент отказа)")
  }
  if (url.includes("/api/query/sql")) {
    const stmt = String(body.stmt ?? "")
    calls.push({ kind: "rest", name: "sql" })
    if (stmt.includes("select markdown from blocks")) {
      return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] })
    }
    return json({ code: 0, msg: "", data: [] })
  }
  if (url.includes("/api/filetree/") || url.includes("/api/opencode") || url.includes("opencode")) {
    throw new Error(`mock: неожиданный REST ${url}`)
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
            { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
          ],
          rows: [{
            id: ROW_ID,
            cells: [
              { value: { keyID: TITLE_KEY, type: "block", block: { content: "мастер крон", refSubtype: "d", id: BODY_DOC } } },
              { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "Вейт" }] } },
            ],
          }],
          rowCount: 1,
        } } })
      }
      // страница 2 — терминатор пагинации (rowCount 0): и начальный срез,
      // и гейт-срез видят строку на странице 1
      return sse({ structuredContent: { view: {
        columns: [],
        rows: [],
        rowCount: 0,
      } } })
    }
    throw new Error(`mock: неожиданная mcp-тулза ${params.name}`)
  }
  throw new Error(`mock: неожиданный mcp-метод ${String(body.method)}`)
}) as typeof fetch

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })
const sse = (result: unknown, sessionId?: string): Response =>
  new Response(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: 1, result })}\n\n`,
    { status: 200, headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {} })

// ——— прогон ———

const state = { crons: {} }
let error = ""
try {
  // Гейт-срез возвращает пустоту (страница 2 мока) → строка «снята»...
  // для детерминизма refuse-сценария гейт обязан увидеть строку: патчим
  // второй render так же, как первый (page-независимо)
  await cron.tickBoard(state, { docId: BOARD_DOC, dbId: AV_BLOCK }, "Задачи uwuwu", new Date("2026-11-07T09:35:00"))
} catch (e) {
  error = String(e)
}
;(Bun as unknown as { spawn: unknown }).spawn = origSpawn

console.log(JSON.stringify({ error, state: state.crons, calls }))
