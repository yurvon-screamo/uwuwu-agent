// tests/cron-agent-dedup-child.ts — инцидентный сценарий дедупа pendingRun:
// дочерний процесс для cron-agent-dedup.test.ts. Ручная копия файр-конверта
// (стирт оператора, текст разошёлся — frontmatter + другое расписание)
// приземлилась в вечную сессию, у демона pendingRun в полёте → следующий тик
// обязан признать приземление ПО СИГНАТУРЕ рана (rowId) и НЕ дублировать POST:
//   Мастер A: копия приземлилась ПОСЛЕ неудачной попытки (между тиками),
//             в /message, перед ней assistant-эхо команд рана (эхо — не приземление).
//   Мастер B: копия приземлилась ДО штампа pendingRun (окно шире attemptAt,
//             граница lastFiredAt), в /inbox (admission-окно, /message слеп).
// State мастеров предзасеян (T1–T3 файр-флоу покрыты cron-agent-fire-child).
// Mock: SiYuan REST + opencode REST (глобальный fetch) + CLI-спавн (Bun.spawn).
import { mkdirSync } from "node:fs"
import { join } from "node:path"

const ROOT = "/tmp/opencode/cron-agent-dedup-home"
const LOG = join(ROOT, "cron-dedup-child.log")
process.env.UWUWU_CRON_LOG = LOG
process.env.UWUWU_CRON_STATE_DIR = join(ROOT, "state")
process.env.UWUWU_CLI_BIN = "uwuwu-cli-fire-mock"
mkdirSync(join(ROOT, "state"), { recursive: true })

const cron = await import("../cron/cron")
const { agentFirePrompt } = await import("../cron/cron-lib")

const AGENT = "test-agent-fire"
const TITLE_KEY = "k-title"
const STATUS_KEY = "k-status"

// ——— времена: фиксированная база тиков, не Date.now() (тики оркестратора
// фейковые — реальная стена ушла бы вперёд и сломала бы сценарий).
// Date.UTC, не локальная дата: родитель-тест сверяет эти числа в своём
// процессе (bun test — TZ=UTC), ребёнок наследует env без TZ — конвенция
// должна быть одинаковой на любой машине/таймзоне ———
const base = Date.UTC(2026, 10, 9, 10, 0, 0)
const M = 60_000
export const LAST_FIRED = base + 5 * M   // прошлый удачный файр
export const ATTEMPT_AT = base + 10 * M  // неудачная попытка, штамп pendingRun

// ——— раны и конверты: сохранённый промпт строит реальный agentFirePrompt
// (как демона), ручные копии — снапшот task get с ДРУГИМ расписанием;
// единственное общее с сохранённым промптом: rowId рана ———
const RUN_A = "20261109103000-rowincA"
const RUN_B = "20261109103000-rowwinB"
const manualCopy = (runId: string, body: string) =>
  `---\ntitle: ⏰ Файр крона «Ночной аудит» (ручной стирт оператора)\ndate: 2026-11-09\n---\n\n⏰ Файр крона «Ночной аудит» (расписание: 30 4 * * *).\nРан-карточка запуска — задача ${runId} на доске «Задачи uwuwu»: она уже создана, работать по ней не нужно.\n\n=== ИНСТРУКЦИЯ (тело мастера) ===\n${body}\n=== КОНЕЦ ИНСТРУКЦИИ ===\n`

// ——— управляемый opencode-сервер ———
interface PromptCall { sessionID: string; text: string; delivery: unknown }
const creations: Record<string, unknown>[] = []
const promptCalls: PromptCall[] = []
const spawnCalls: string[][] = []
// A — landing в /message (assistant-эхо на 100мс раньше user-копии);
// B — landing в /inbox на 5с ДО штампа pendingRun (старое окно attemptAt−2000
// её теряло бы, граница lastFiredAt видит)
const msgBoxes: Record<string, unknown[]> = {
  ses_manual_a: [
    { type: "assistant", time: { created: ATTEMPT_AT + 100 }, content: [{ type: "text", text: `uwuwu-cli task edit ${RUN_A} --project uwuwu --status Todo` }] },
    { type: "user", time: { created: ATTEMPT_AT + 500 }, text: manualCopy(RUN_A, "аудит досок") },
  ],
  ses_manual_b: [],
}
const inboxBoxes: Record<string, unknown[]> = {
  ses_manual_a: [],
  ses_manual_b: [{ type: "user", time: { created: ATTEMPT_AT - 5000 }, payload: { text: manualCopy(RUN_B, "аудит досок B") } }],
}

const json = (data: unknown): Response => new Response(JSON.stringify(data), { status: 200 })
const sse = (result: unknown, sessionId?: string): Response =>
  new Response(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: 1, result })}\n\n`,
    { status: 200, headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {} })

globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input)
  const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>

  // — opencode REST (боевой контур) —
  if (url.endsWith("/api/session") && init?.method === "POST") {
    creations.push(body)
    return json({ id: `ses_new_${creations.length}` })
  }
  const promptMatch = /\/api\/session\/(ses_[^/]+)\/prompt$/.exec(url)
  if (promptMatch !== null) {
    promptCalls.push({ sessionID: promptMatch[1], text: String(body.text ?? ""), delivery: body.delivery })
    return json({ time: { created: Date.now() } })
  }
  if (/\/api\/session\/ses_[^/]+\/?$/.test(url)) return json({ outcome: undefined, time: { idle: 123 } })
  const inboxMatch = /\/api\/session\/(ses_[^/]+)\/inbox$/.exec(url)
  if (inboxMatch !== null) return json(inboxBoxes[inboxMatch[1]] ?? [])
  const msgMatch = /\/api\/session\/(ses_[^/]+)\/message/.exec(url)
  if (msgMatch !== null) return json(msgBoxes[msgMatch[1]] ?? [])

  // — SiYuan REST/MCP (доска с двумя мастерами) —
  if (url.includes("/api/attr/getBlockAttrs")) {
    return json({ code: 0, msg: "", data: { "custom-schedule": "*/5 * * * *", "custom-agent": AGENT, title: "fire-мастер" } })
  }
  if (url.includes("/api/export/exportMdContent")) {
    return json({ code: 0, msg: "", data: { content: "---\ntitle: fire-мастер\n---\n\n# fire-мастер\n\nПроверь audit-контур." } })
  }
  if (url.includes("/api/block/appendBlock")) {
    throw new Error("mock: дедуп приземления не должен комментить и спавнить")
  }
  if (url.includes("/api/query/sql")) {
    if (String(body.stmt ?? "").includes("select markdown from blocks")) {
      return json({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="20260101000000-abcdef"></div>` }] })
    }
    return json({ code: 0, msg: "", data: [] })
  }
  if (!url.includes("/mcp")) throw new Error(`mock: неожиданный REST ${url}`)
  if (body.method === "initialize") return sse({}, "s1")
  if (body.method === "notifications/initialized") return new Response("", { status: 200 })
  if (body.method === "tools/call") {
    const params = body.params as { name?: string; arguments?: { action?: string } }
    if (params.name === "database" && params.arguments?.action === "render") {
      const row = (id: string, title: string, bodyDoc: string) => ({
        id,
        cells: [
          { value: { keyID: TITLE_KEY, type: "block", block: { content: title, refSubtype: "d", id: bodyDoc } } },
          { value: { keyID: STATUS_KEY, type: "mSelect", mSelect: [{ content: "Вейт" }] } },
        ],
      })
      return sse({ structuredContent: { view: {
        columns: [
          { id: TITLE_KEY, name: "Title" }, { id: STATUS_KEY, name: "Status" },
        ],
        rows: [
          row("20261109000000-masterA", "мастер A (инцидент)", "20261109000000-docA"),
          row("20261109000000-masterB", "мастер B (окно)", "20261109000000-docB"),
        ],
        rowCount: 2,
      } } })
    }
    throw new Error(`mock: неожиданная mcp-тулза ${params.name}`)
  }
  throw new Error(`mock: неожиданный mcp-метод ${String(body.method)}`)
}) as typeof fetch

// ——— mock CLI: reconcile клеток (task edit) ок; task create = дубль-ран —
// фиксируется в отчёте, по контракту не должен звониться ———
;(Bun as unknown as { spawn: unknown }).spawn = ((argv: unknown) => {
  const a = argv as string[]
  spawnCalls.push(a)
  const out = a[2] === "create"
    ? `создана: дубль-ран!\n  строка: 20261109000000-DUP-RUN\n  тело: 20261109000000-dupdoc`
    : `обновлено: ${a[3]}`
  return {
    stdin: { write: () => {}, end: () => {} },
    stdout: new Response(out).body,
    stderr: new Response("").body,
    exited: Promise.resolve(0),
  }
}) as typeof Bun.spawn

// ——— прогон: два мастера с pendingRun в полёте, один тик (недуевой —
// ретрай обязан отработать до due-гейта) ———
const state = { crons: {
  "20261109000000-masterA": { sessionID: "ses_manual_a", lastFiredAt: LAST_FIRED, firedAtIso: new Date(LAST_FIRED).toISOString(), pendingRun: { rowId: RUN_A, prompt: agentFirePrompt({ title: "Ночной аудит", schedule: "*/5 * * * *" }, "аудит досок", RUN_A, "uwuwu"), attemptAt: ATTEMPT_AT } },
  "20261109000000-masterB": { sessionID: "ses_manual_b", lastFiredAt: LAST_FIRED, firedAtIso: new Date(LAST_FIRED).toISOString(), pendingRun: { rowId: RUN_B, prompt: agentFirePrompt({ title: "Ночной аудит B", schedule: "*/5 * * * *" }, "аудит досок B", RUN_B, "uwuwu"), attemptAt: ATTEMPT_AT } },
} as Record<string, unknown> }

let error = ""
try {
  await cron.tickBoard(state as never, { docId: "20261109000000-board1", dbId: "20261109000000-avblk1" }, "Задачи uwuwu", new Date(base + 17 * M))
} catch (e) {
  error = String(e)
}
;(Bun as unknown as { spawn: unknown }).spawn = Bun.spawn

let logLines: string[] = []
try {
  logLines = (await Bun.file(LOG).text()).split("\n").filter((l) => l.trim() !== "")
} catch {}

const masterOf = (row: string) => state.crons[row] as Record<string, unknown>
console.log(JSON.stringify({
  error,
  promptCalls,
  creations,
  createCalls: spawnCalls.filter((a) => a[2] === "create"),
  masterA: { pendingRun: masterOf("20261109000000-masterA").pendingRun ?? null, lastFiredAt: masterOf("20261109000000-masterA").lastFiredAt },
  masterB: { pendingRun: masterOf("20261109000000-masterB").pendingRun ?? null, lastFiredAt: masterOf("20261109000000-masterB").lastFiredAt },
  logLines,
}))
