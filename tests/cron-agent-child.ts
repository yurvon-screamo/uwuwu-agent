// tests/cron-agent-child.ts — дочерний процесс для cron-agent-dryrun.test.ts:
// DRY_RUN тика вечного мастера (custom-agent). Два сценария одной доской:
// агент без дефа (~/.config/opencode/agents/<name>.md отсутствует) — отказ
// без записей; агент с дефом — позитивная ветка (лог решений, ноль мутаций
// и ноль opencode-REST: файр до POST не доходит). Env задаётся до импорта
// cron.ts — паттерн cron-dryrun-child.ts.
import { mkdirSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const LOG = "/tmp/opencode/cron-agent-home/cron-agent-child.log"
process.env.UWUWU_CRON_LOG = LOG
process.env.UWUWU_CRON_STATE_DIR = "/tmp/opencode/cron-agent-home/state"
process.env.UWUWU_CRON_DRY_RUN = "1"
mkdirSync("/tmp/opencode/cron-agent-home/state", { recursive: true })

// деф позитивного агента: имя с pid-суффиксом (машинные дефы не перезаписать),
// frontmatter — инлайн #variant (покрыт парсером модели)
const AGENT_OK = `test-agent-y-${process.pid}`
const AGENT_DIR = join(homedir(), ".config/opencode/agents")
mkdirSync(AGENT_DIR, { recursive: true })
writeFileSync(join(AGENT_DIR, `${AGENT_OK}.md`), "---\nmode: primary\nmodel: zai-coding-plan/test-model#max\n---\n\n# Тестовый агент\n")
const AGENT_MISSING = `missing-agent-x-${process.pid}`

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261109000000-board1"
const AV_BLOCK = "20261109000000-avblk1"
const ROW_MISSING = "20261109000000-rowmiss"
const ROW_OK = "20261109000000-rowokok"
const BODY_MISSING = "20261109000000-bmiss01"
const BODY_OK = "20261109000000-bokok01"

interface Call {
  kind: "mcp-tool" | "rest" | "spawn"
  name: string
}
const calls: Call[] = []

// CLI не зовётся в DRY_RUN (ран-карточка не создаётся)
const origSpawn = Bun.spawn
;(Bun as unknown as { spawn: unknown }).spawn = ((cmd: unknown) => {
  calls.push({ kind: "spawn", name: String(Array.isArray(cmd) ? cmd[0] : cmd) })
  throw new Error(`mock: DRY_RUN не должен спавнить процессы (${String(cmd)})`)
}) as typeof Bun.spawn

const rows = [
  {
    id: ROW_MISSING,
    titleRef: BODY_MISSING,
    title: "мастер-missing",
    agent: AGENT_MISSING,
    doc: BODY_MISSING,
  },
  {
    id: ROW_OK,
    titleRef: BODY_OK,
    title: "мастер-ok",
    agent: AGENT_OK,
    doc: BODY_OK,
  },
]

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>
  if (url.includes("/api/attr/getBlockAttrs")) {
    calls.push({ kind: "rest", name: "getBlockAttrs" })
    const doc = (body.id as string | undefined) ?? ""
    const row = rows.find((r) => r.doc === doc)
    return json({
      code: 0, msg: "",
      data: { "custom-schedule": "*/5 * * * *", "custom-agent": row?.agent ?? "", title: row?.title ?? "" },
    })
  }
  if (url.includes("/api/export/exportMdContent")) {
    calls.push({ kind: "rest", name: "exportMdContent" })
    return json({ code: 0, msg: "", data: { content: "---\ntitle: мастер\n---\n\n# мастер\n\nПроверь audit-контур." } })
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
  if (url.includes("/api/filetree/") || url.includes("/api/opencode")) {
    throw new Error(`mock: неожиданный REST ${url}`)
  }
  if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url}`)
  if (body.method === "initialize") return sse({}, "s1")
  if (body.method === "notifications/initialized") return new Response("", { status: 200 })
  if (body.method === "tools/call") {
    const params = body.params as { name?: string; arguments?: { action?: string; page?: number } }
    calls.push({ kind: "mcp-tool", name: `${params.name}:${params.arguments?.action ?? "?"}` })
    if (params.name === "database" && params.arguments?.action === "render") {
      // page-независимо: и начальный срез, и fireGate-срез видят обе строки
      return sse({ structuredContent: { view: {
        columns: [
          { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
        ],
        rows: rows.map((r) => ({
          id: r.id,
          cells: [
            { value: { keyID: TITLE_KEY, type: "block", block: { content: r.title, refSubtype: "d", id: r.titleRef } } },
            { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "Вейт" }] } },
          ],
        })),
        rowCount: rows.length,
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
  await cron.tickBoard(state, { docId: BOARD_DOC, dbId: AV_BLOCK }, "Задачи uwuwu", new Date("2026-11-09T09:35:00"))
} catch (e) {
  error = String(e)
}
;(Bun as unknown as { spawn: unknown }).spawn = origSpawn

let logText = ""
try { logText = await Bun.file(LOG).text() } catch {}
const logLines = logText.split("\n").filter((l) => l.trim() !== "")

console.log(JSON.stringify({ error, state: state.crons, calls, logLines, agentOk: AGENT_OK, agentMissing: AGENT_MISSING }))
