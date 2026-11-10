// tests/cron-agent-drift-child.ts — дрейф агента вечной сессии (смена
// custom-agent карточки при живой сессии, эпизод 09.11: тихий «Agent not
// found» без ретрая). Дочерний процесс для cron-agent-drift.test.ts.
// Семь тиков одной доской (расписание */5):
//   T1 штатный файр (агент A) → ses_mock_1, POST ok;
//   T2 смена карточки на B при idle-сессии → recreate: ses_mock_2 (агент B),
//      DELETE ses_mock_1, конверт в новой сессии, лог «агент сменён»;
//   T3 штатный файр без смены → без recreate (регресс), prompt ses_mock_2;
//   T4 смена на C при живом ходе → defer: steer в ses_mock_2, без recreate;
//   T5 смена после idle → recreate: ses_mock_3 (агент C), DELETE ses_mock_2;
//   T6 failed POST → pendingRun (ран не теряется);
//   T7 ретрай при новом дрейфе (B) → recreate ses_mock_4 (агент B), исходный
//      конверт в новую сессию, ран НЕ дублируется;
//   T8 failed POST → новый pendingRun (ран run7);
//   T9 ретрай при дрейфе на агент БЕЗ дефа → refuseOnce (коммент в мастер),
//      recreate НЕ случается (мёртвой сессии нет), pendingRun жив (слот не
//      сжигается), повторных комментов при том же отказе нет;
//   T10 возврат агента с дефом → ретрай приземляет ран run7, pendingRun
//      снят, lastRefusal сброшен (дедуп refuseOnce открыт).
// Mock: SiYuan REST + opencode REST (глобальный fetch) + CLI-спавн
// (Bun.spawn); registry сессий несёт agent/outcome/idle — форма GET /session
// живого сервера (живая проба 10.11). HOME-песочница задаётся спавнером.
import { mkdirSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const ROOT = "/tmp/opencode/cron-agent-drift-home"
const LOG = join(ROOT, "cron-drift-child.log")
process.env.UWUWU_CRON_LOG = LOG
process.env.UWUWU_CRON_STATE_DIR = join(ROOT, "state")
process.env.UWUWU_CLI_BIN = "uwuwu-cli-drift-mock"
mkdirSync(join(ROOT, "state"), { recursive: true })

// виртуальные часы: Date.now прод-кода держится внутри окна тиков (10:01
// против тиков 10:05–10:31) — компакт-проба видит отрицательный idleMs и
// не стреляет; lastFiredAt пишется от времён тиков мока, not real wall.
const VIRTUAL_NOW = new Date(2026, 10, 9, 10, 1, 0).getTime()
Date.now = () => VIRTUAL_NOW

// дефы трёх агентов в HOME-песочнице (frontmatter с инлайн #variant)
const AGENT_A = "test-drift-a"
const AGENT_B = "test-drift-b"
const AGENT_C = "test-drift-c"
const agentsDir = join(homedir(), ".config/opencode/agents")
mkdirSync(agentsDir, { recursive: true })
for (const name of [AGENT_A, AGENT_B, AGENT_C]) {
  writeFileSync(join(agentsDir, `${name}.md`), `---\nmode: primary\nmodel: zai-coding-plan/test-model#max\n---\n\n# ${name}\n`)
}

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261109000000-board1"
const AV_BLOCK = "20261109000000-avblk1"
const ROW = "20261109000000-master"
const BODY = "20261109000000-masterdoc"

// ——— управляемый opencode-сервер ———

// custom-agent карточки: оркестратор переключает между тиками (легальная
// операция владельца — эпизод 09.11)
let agentAttr = AGENT_A
interface MockSession { agent: string; outcome: unknown; idle: number }
const sessions = new Map<string, MockSession>()
const creations: { title: string; agent: unknown; model: unknown }[] = []
interface PromptCall { sessionID: string; text: string; delivery: unknown }
const promptCalls: PromptCall[] = []
const deletes: string[] = []
const comments: { docId: string; content: string }[] = []
let failNextPost = false
// времена приземления POST — от времён тиков оркестратора, не от Date.now()
let currentTickMs = 0

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const method = init?.method ?? "GET"
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>

  // — opencode REST (боевой контур) —
  if (url.endsWith("/api/session") && method === "POST") {
    const id = `ses_mock_${creations.length + 1}`
    creations.push({ title: String(body.title ?? ""), agent: body.agent, model: body.model })
    sessions.set(id, { agent: String(body.agent ?? ""), outcome: undefined, idle: 0 })
    return json({ id })
  }
  if (/\/api\/session\/ses_[^/]+$/.test(url) && method === "DELETE") {
    const id = /\/api\/session\/(ses_[^/]+)$/.exec(url)?.[1] ?? ""
    deletes.push(id)
    sessions.delete(id)
    return json({})
  }
  const promptMatch = /\/api\/session\/(ses_[^/]+)\/prompt$/.exec(url)
  if (promptMatch !== null) {
    promptCalls.push({ sessionID: promptMatch[1], text: String(body.text ?? ""), delivery: body.delivery })
    if (failNextPost) return new Response("boom: POST не приземлился", { status: 500 })
    const created = currentTickMs
    const s = sessions.get(promptMatch[1])
    if (s !== undefined) {
      s.outcome = "succeeded" // ход дошёл до idle к следующему тику
      s.idle = created + 1000
    }
    return json({ time: { created } })
  }
  const getMatch = /\/api\/session\/(ses_[^/]+)\/?$/.exec(url)
  if (getMatch !== null) {
    const s = sessions.get(getMatch[1])
    if (s === undefined) return new Response("нет сессии", { status: 404 })
    return json({ outcome: s.outcome, time: { idle: s.idle }, agent: s.agent })
  }
  if (url.includes("/message")) return json([])
  if (url.includes("/inbox")) return new Response("нет inbox", { status: 404 })

  // — SiYuan REST/MCP (доска мастера) —
  if (url.includes("/api/attr/getBlockAttrs")) {
    return json({ code: 0, msg: "", data: { "custom-schedule": "*/5 * * * *", "custom-agent": agentAttr, title: "drift-мастер" } })
  }
  if (url.includes("/api/export/exportMdContent")) {
    return json({ code: 0, msg: "", data: { content: "---\ntitle: drift-мастер\n---\n\n# drift-мастер\n\nПроверь audit-контур." } })
  }
  if (url.includes("/api/block/appendBlock")) {
    // refuseOnce (T9: дрейф на агент без дефа) — трекаем коммент-отказ
    comments.push({ docId: String(body.parentID ?? ""), content: String(body.data ?? "") })
    return json({ code: 0, msg: "", data: [] })
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
      return sse({ structuredContent: { view: {
        columns: [
          { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
        ],
        rows: [{
          id: ROW,
          cells: [
            { value: { keyID: TITLE_KEY, type: "block", block: { content: "drift-мастер", refSubtype: "d", id: BODY } } },
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

// ——— прогон: семь тиков ———

interface Stage { label: string; state: Record<string, unknown>; prompts: number; creations: number; spawns: number; runCards: number; deletes: number }
const stages: Stage[] = []
const state = { crons: {} as Record<string, unknown> }
let error = ""
const snap = (label: string) => stages.push({
  label, state: structuredClone(state.crons),
  prompts: promptCalls.length, creations: creations.length, spawns: spawnCalls.length,
  runCards: spawnCalls.filter((c) => c.argv[2] === "create").length,
  deletes: deletes.length,
})

try {
  const base = new Date(2026, 10, 9, 10, 0, 0).getTime()
  const board = { docId: BOARD_DOC, dbId: AV_BLOCK }
  const tick = async (min: number) => {
    currentTickMs = base + min * 60_000
    await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(currentTickMs))
  }
  await tick(5) // T1: штатный файр, агент A
  snap("T1")
  agentAttr = AGENT_B // владелец сменил custom-agent карточки
  await tick(10) // T2: idle-сессия вшила A → recreate под B
  snap("T2")
  await tick(15) // T3: без смены → обычный файр в ses_mock_2 (регресс recreate)
  snap("T3")
  agentAttr = AGENT_C // смена при живом ходе
  const live = sessions.get("ses_mock_2")
  if (live === undefined) throw new Error("mock: ses_mock_2 не создан — сценарий сломан")
  live.outcome = undefined // ход в процессе
  await tick(20) // T4: defer — steer в живую сессию, без recreate
  snap("T4")
  live.outcome = "succeeded" // ход дошёл до idle
  live.idle = currentTickMs + 1000
  await tick(25) // T5: дрейф на idle → recreate под C
  snap("T5")
  failNextPost = true
  await tick(30) // T6: файр в ses_mock_3 → POST падает → pendingRun
  snap("T6")
  failNextPost = false
  agentAttr = AGENT_B // ещё одна смена — в окне ретрая
  await tick(31) // T7: ретрай до due-слота: дрейф → recreate, исходный конверт в новую сессию
  snap("T7")
  failNextPost = true
  await tick(35) // T8: файр в ses_mock_4 → POST падает → pendingRun (ран run7)
  snap("T8")
  failNextPost = false
  agentAttr = "test-drift-missing" // деф не создавался — resolveAgentIssue обязан отказать
  await tick(36) // T9: ретрай при дрейфе B→missing: гейт дефа → refuseOnce, recreate НЕТ
  snap("T9")
  agentAttr = AGENT_B // владелец вернул агент с дефом
  await tick(37) // T10: ретрай без дрейфа → ран run7 приземляется, дедуп refusal сброшен
  snap("T10")
} catch (e) {
  error = String(e)
}

;(Bun as unknown as { spawn: unknown }).spawn = Bun.spawn

let logLines: string[] = []
try {
  logLines = (await Bun.file(LOG).text()).split("\n").filter((l) => l.trim() !== "")
} catch {}

console.log(JSON.stringify({
  error, stages, creations, promptCalls, deletes, spawnCalls, logLines, comments,
  agentsDir, masterDocId: BODY, agents: [AGENT_A, AGENT_B, AGENT_C],
  runCardIds: spawnCalls
    .filter((c) => c.argv[2] === "create")
    .map((c) => /строка: (\S+)/.exec(c.stdoutText)?.[1] ?? ""),
}))
