// tests/cron-agent-fire-child.ts — боевой (не-DRY_RUN) файр вечного мастера:
// дочерний процесс для cron-agent-fire.test.ts. Четыре тика одной доской:
//   T1 gone → вечная сессия создана (agent + модель из дефа) → ран-карточка →
//      POST ok → state (sessionID, lastFiredAt, без pendingRun);
//   T2 сессия running → стирт (delivery:"steer");
//   T3 POST промпта падает → pendingRun {rowId, prompt, attemptAt} в state,
//      lastFiredAt не сжигается;
//   T4 ретрай ДО due-слота: промпт приземлился ранее (/message) → файр
//      завершён без повторного POST (дубль-инструкция не доезжает).
// Mock: SiYuan REST + opencode REST (глобальный fetch) + CLI-спавн (Bun.spawn).
// HOME-песочница задаётся спавнером: деф агента child пишет в неё же,
// service.json туда кладёт спавнер (restCall читает реестр на каждый вызов).
import { mkdirSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const ROOT = "/tmp/opencode/cron-agent-fire-home"
const LOG = join(ROOT, "cron-fire-child.log")
process.env.UWUWU_CRON_LOG = LOG
process.env.UWUWU_CRON_STATE_DIR = join(ROOT, "state")
process.env.UWUWU_CLI_BIN = "uwuwu-cli-fire-mock"
mkdirSync(join(ROOT, "state"), { recursive: true })

// деф позитивного агента в HOME-песочнице: frontmatter с инлайн #variant
// (покрывает парсер модели) — создания машинных дефов не будет
const AGENT = "test-agent-fire"
const AGENT_PATH = join(homedir(), ".config/opencode/agents", `${AGENT}.md`)
mkdirSync(join(homedir(), ".config/opencode/agents"), { recursive: true })
writeFileSync(AGENT_PATH, "---\nmode: primary\nmodel: zai-coding-plan/test-model#max\n---\n\n# Тестовый агент\n")

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261109000000-board1"
const AV_BLOCK = "20261109000000-avblk1"
const ROW = "20261109000000-master"
const BODY = "20261109000000-masterdoc"

// ——— управляемый opencode-сервер ———

interface Creation { title: string; agent: unknown; model: unknown }
const creations: Creation[] = []
interface PromptCall { sessionID: string; text: string; delivery: unknown }
const promptCalls: PromptCall[] = []
// исходы POST по порядку: T1 ok, T2 ok (стирт), T3 fail (pendingRun); T4 — POST не звать
const promptOutcomes: ("ok" | "fail")[] = ["ok", "ok", "fail"]
// GET /session/<id>: outcome undefined → sessionStatus = running (стирт)
const sessionState: { outcome: unknown; idle: number } = { outcome: undefined, idle: 123 }
// user-сообщение для дедупа приземления на T4 (заполняет оркестратор)
let landing: { created: number; text: string } | null = null
let createdTick = Date.now()

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>

  // — opencode REST (боевой контур) —
  if (url.endsWith("/api/session") && init?.method === "POST") {
    creations.push({
      title: String(body.title ?? ""), agent: body.agent, model: body.model,
    })
    return json({ id: `ses_mock_${creations.length}` })
  }
  const promptMatch = /\/api\/session\/(ses_[^/]+)\/prompt$/.exec(url)
  if (promptMatch !== null) {
    const mode = promptOutcomes[promptCalls.length] ?? "ok"
    promptCalls.push({ sessionID: promptMatch[1], text: String(body.text ?? ""), delivery: body.delivery })
    if (mode === "fail") return new Response("boom: POST не приземлился", { status: 500 })
    createdTick += 1
    return json({ time: { created: createdTick } })
  }
  if (/\/api\/session\/ses_[^/]+\/?$/.test(url)) return json({ outcome: sessionState.outcome, time: { idle: sessionState.idle } })
  if (url.includes("/message")) {
    return json(landing === null ? [] : [{ type: "user", time: { created: landing.created }, text: landing.text }])
  }
  if (url.includes("/inbox")) return new Response("нет inbox", { status: 404 })

  // — SiYuan REST/MCP (доска мастера) —
  if (url.includes("/api/attr/getBlockAttrs")) {
    return json({ code: 0, msg: "", data: { "custom-schedule": "*/5 * * * *", "custom-agent": AGENT, title: "fire-мастер" } })
  }
  if (url.includes("/api/export/exportMdContent")) {
    return json({ code: 0, msg: "", data: { content: "---\ntitle: fire-мастер\n---\n\n# fire-мастер\n\nПроверь audit-контур." } })
  }
  if (url.includes("/api/block/appendBlock")) {
    throw new Error("mock: боевой файр не должен комментить отказ (деф на месте)")
  }
  if (url.includes("/api/query/sql")) {
    if (String(body.stmt ?? "").includes("select markdown from blocks")) {
      return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] })
    }
    return json({ code: 0, msg: "", data: [] })
  }
  if (!url.includes("/mcp")) throw new Error(`mock: неожиданный REST ${url}`)
  if (body.method === "initialize") return sse({}, "s1")
  if (body.method === "notifications/initialized") return new Response("", { status: 200 })
  if (body.method === "tools/call") {
    const params = body.params as { name?: string; arguments?: { action?: string } }
    if (params.name === "database" && params.arguments?.action === "render") {
      // page-независимо: начальный срез и fireGate-срез видят одну Вейт-строку
      return sse({ structuredContent: { view: {
        columns: [
          { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
        ],
        rows: [{
          id: ROW,
          cells: [
            { value: { keyID: TITLE_KEY, type: "block", block: { content: "fire-мастер", refSubtype: "d", id: BODY } } },
            { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "Вейт" }] } },
          ],
        }],
        rowCount: 1,
      } } })
    }
    throw new Error(`mock: неожиданная mcp-тулза ${params.name}`)
  }
  throw new Error(`mock: неожиданный mcp-метод ${String(body.method)}`)
}) as typeof fetch

const sse = (result: unknown, sessionId?: string): Response =>
  new Response(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: 1, result })}\n\n`,
    { status: 200, headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {} })

// ——— mock CLI (ран-карточка + reconcile клеток) ———

interface SpawnCall { argv: string[]; stdinWritten: string; stdoutText: string }
const spawnCalls: SpawnCall[] = []
let runCounter = 0

const mockProc = (stdoutText: string, opts: { stdin?: string } | undefined, call: SpawnCall) => {
  call.stdoutText = stdoutText
  return {
    stdin: {
      write: (chunk: string) => {
        if (opts?.stdin === "pipe") call.stdinWritten = chunk
      },
      end: () => {},
    },
    stdout: new Response(stdoutText).body,
    stderr: new Response("").body,
    exited: Promise.resolve(0),
  }
}

;(Bun as unknown as { spawn: unknown }).spawn = ((argv: unknown, opts?: { stdin?: string }) => {
  const a = argv as string[]
  const call: SpawnCall = { argv: a, stdinWritten: "", stdoutText: "" }
  spawnCalls.push(call)
  if (a[1] === "task" && a[2] === "create") {
    runCounter += 1
    const rowId = `20261109000000-run${runCounter}`
    const title = a[a.indexOf("--title") + 1] ?? "?"
    return mockProc(`создана: ${title}\n  строка: ${rowId}\n  тело: 20261109000000-rundoc${runCounter}`, opts, call)
  }
  if (a[1] === "task" && a[2] === "edit") return mockProc(`обновлено: ${a[3]}`, opts, call)
  throw new Error(`mock: неожиданный спавн ${a.join(" ")}`)
}) as typeof Bun.spawn

// ——— прогон: четыре тика ———

interface Stage { label: string; state: Record<string, unknown>; prompts: number; creations: number; spawns: number; runCards: number }
const stages: Stage[] = []
const state = { crons: {} as Record<string, unknown> }
let error = ""
const snap = (label: string) => stages.push({
  label, state: structuredClone(state.crons),
  prompts: promptCalls.length, creations: creations.length, spawns: spawnCalls.length,
  runCards: spawnCalls.filter((c) => c.argv[2] === "create").length,
})

try {
  const base = new Date(2026, 10, 9, 10, 0, 0).getTime()
  const board = { docId: BOARD_DOC, dbId: AV_BLOCK }
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 5 * 60_000))
  snap("T1")
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 10 * 60_000))
  snap("T2")
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 15 * 60_000))
  snap("T3")
  // T4: теперь недуевой слот (+2м от файра T3) — pendingRun-ретрай обязан
  // отработать и до due-гейта
  const pending = (state.crons[ROW] as { pendingRun?: { attemptAt: number; prompt: string } }).pendingRun
  if (pending === undefined) throw new Error("mock: T3 не оставил pendingRun — сценарий сломан")
  landing = { created: pending.attemptAt + 500, text: pending.prompt }
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 17 * 60_000))
  snap("T4")
} catch (e) {
  error = String(e)
}

;(Bun as unknown as { spawn: unknown }).spawn = Bun.spawn

let logLines: string[] = []
try {
  logLines = (await Bun.file(LOG).text()).split("\n").filter((l) => l.trim() !== "")
} catch {}

console.log(JSON.stringify({
  error, stages, creations, promptCalls, spawnCalls, logLines,
  agentsDir: join(homedir(), ".config/opencode/agents"), agent: AGENT,
  runCardIds: spawnCalls
    .filter((c) => c.argv[2] === "create")
    .map((c) => /строка: (\S+)/.exec(c.stdoutText)?.[1] ?? ""),
  landingPrompt: landing?.text ?? "",
}))
