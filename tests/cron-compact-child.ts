// tests/cron-compact-child.ts — автокомпакт вечной сессии в тике крона:
// дочерний процесс для cron-compact.test.ts. Мастер НЕ due (расписание 1d,
// файр был сейчас) — компакт-чек обязан отработать ДО due-гейта:
//   T1 idle+раздутая сессия → POST /compact ровно один, lastCompactAt в state;
//   T2 (+1м) коулдаун прошлой admission → повторного POST нет;
//   T3 (+2м) lastCompactAt состарен, но pendingRun в полёте → чек скипается.
// Mock: SiYuan REST + opencode REST (глобальный fetch) + CLI-спавн (Bun.spawn).
// HOME-песочница задаётся спавнером (деф агента + service.json туда же).
import { mkdirSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const ROOT = "/tmp/opencode/cron-compact-home"
const LOG = join(ROOT, "cron-compact-child.log")
process.env.UWUWU_CRON_LOG = LOG
process.env.UWUWU_CRON_STATE_DIR = join(ROOT, "state")
process.env.UWUWU_CLI_BIN = "uwuwu-cli-compact-mock"
// пороги для прогона: idle 240с (сессия простояла 5ч — проходит), окно чека
// 60с (тики сценария через минуту — троттлинг не съедает шаги)
process.env.UWUWU_CRON_COMPACT_IDLE_SEC = "240"
process.env.UWUWU_CRON_COMPACT_CHECK_SEC = "60"
mkdirSync(join(ROOT, "state"), { recursive: true })

// деф агента в HOME-песочнице: гейт resolveAgentIssue смотрит только
// существование файла (модель компакт-чеку не нужна — берётся из сессии)
const AGENT = "test-agent-compact"
mkdirSync(join(homedir(), ".config/opencode/agents"), { recursive: true })
writeFileSync(join(homedir(), ".config/opencode/agents", `${AGENT}.md`), "---\nmode: primary\n---\n\n# Тестовый агент\n")

const cron = await import("../cron/cron")

const AV_ID = "20260101000000-abcdef"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"
const BOARD_DOC = "20261110000000-board1"
const AV_BLOCK = "20261110000000-avblk1"
const ROW = "20261110000000-master"
const BODY = "20261110000000-masterdoc"
const SESSION = "ses_mock_1"

// ——— управляемый opencode-сервер ———

const compactCalls: string[] = []
const sessionModel: Record<string, unknown> = {
  providerID: "zai-coding-plan", id: "test-model",
}
let idleAt: number | null = Date.now() - 5 * 3_600_000 // idle 5ч ≥ порога (финальная метка — в сценарии ниже)
const messages: unknown[] = [
  { type: "assistant", tokens: { input: 500_000, output: 0, reasoning: 0, cache: { read: 0, write: 0 } } },
]

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>

  // — opencode REST (боевой контур) —
  const compactMatch = /\/api\/session\/(ses_[^/]+)\/compact$/.exec(url)
  if (compactMatch !== null) {
    compactCalls.push(compactMatch[1])
    return json({ id: "msg_compact", sessionID: compactMatch[1], type: "compaction", time: { created: Date.now() } })
  }
  const promptMatch = /\/api\/session\/(ses_[^/]+)\/prompt$/.exec(url)
  if (promptMatch !== null) {
    // pendingRun-ретрай (сценарий T3): промпт доигрывается, compact не трогаем
    return json({ time: { created: Date.now() } })
  }
  if (/\/api\/session\/ses_[^/]+\/?$/.test(url)) {
    return json({ outcome: "succeeded", time: { idle: idleAt }, model: sessionModel })
  }
  if (url.includes("/message")) return json(messages)
  if (url.endsWith("/api/model")) {
    return json({ data: [{ providerID: "zai-coding-plan", modelID: "test-model", limit: { context: 1_000_000, output: 131_072 } }] })
  }
  if (url.includes("/inbox")) return new Response("нет inbox", { status: 404 })

  // — SiYuan REST/MCP (доска мастера) —
  if (url.includes("/api/attr/getBlockAttrs")) {
    return json({ code: 0, msg: "", data: { "custom-schedule": "1d", "custom-agent": AGENT, title: "compact-мастер" } })
  }
  if (url.includes("/api/export/exportMdContent")) {
    return json({ code: 0, msg: "", data: { content: "---\ntitle: compact-мастер\n---\n\n# compact-мастер\n\nПроверь контур." } })
  }
  if (url.includes("/api/block/appendBlock")) {
    throw new Error("mock: компакт-тик не должен комментить (мастер не due, отказов нет)")
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
            { value: { keyID: TITLE_KEY, type: "block", block: { content: "compact-мастер", refSubtype: "d", id: BODY } } },
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

// ——— mock CLI (reconcile клеток мастера) ———

const spawnCalls: string[][] = []
;(Bun as unknown as { spawn: unknown }).spawn = ((argv: unknown) => {
  const a = argv as string[]
  spawnCalls.push(a.map(String))
  return {
    stdout: new Response("обновлено\n").body,
    stderr: new Response("").body,
    exited: Promise.resolve(0),
  } as unknown as ReturnType<typeof Bun.spawn>
}) as typeof Bun.spawn

// ——— прогон: три тика недуевого мастера ———

// эпоха сценария: файр был 5ч назад, сессия простаивает с тех пор (idle-метка
// чуть позже файра — граница lastFiredAt подтверждает idle)
const base = Date.now() - 5 * 3_600_000
idleAt = base + 60_000
const state = { crons: {
  [ROW]: { sessionID: SESSION, lastFiredAt: base, firedAtIso: new Date(base).toISOString() },
} as Record<string, unknown> }

interface Stage { label: string; state: Record<string, unknown>; compacts: number }
const stages: Stage[] = []
let error = ""
const snap = (label: string) => stages.push({
  label, state: structuredClone(state.crons), compacts: compactCalls.length,
})

try {
  const board = { docId: BOARD_DOC, dbId: AV_BLOCK }
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 60_000))
  snap("T1")
  // T2: коулдаун — метка admission свежая, POST не повторяется
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 2 * 60_000))
  snap("T2")
  // T3: метка состарена искусственно, но pendingRun в полёте — чек скипается
  const tracked = state.crons[ROW] as { lastCompactAt?: number }
  tracked.lastCompactAt = base - 2 * 3_600_000
  ;(state.crons[ROW] as Record<string, unknown>).pendingRun = { rowId: "20261110000000-run1", prompt: "инструкция", attemptAt: base + 120_000 }
  await cron.tickBoard(state as never, board, "Задачи uwuwu", new Date(base + 3 * 60_000))
  snap("T3")
} catch (e) {
  error = String(e)
}

;(Bun as unknown as { spawn: unknown }).spawn = Bun.spawn

let logLines: string[] = []
try {
  logLines = (await Bun.file(LOG).text()).split("\n").filter((l) => l.trim() !== "")
} catch {}

console.log(JSON.stringify({
  error, stages, compactCalls, spawnCalls, logLines,
  compactEnv: { idleSec: process.env.UWUWU_CRON_COMPACT_IDLE_SEC },
}))
