// factory-stall-gate-child.ts — спавн-ребёнок тик-тестов сталл-ветки и
// bodyEdit-гейта фабрики (задача 20261109183658-kh3e7qe). Сценарий — env
// STALL_GATE_SCENARIO:
//   review_swallow   — строка в Review, контентная правка тела (отчёт
//                      тестировщика): ожидание — проглот без будки дева,
//                      базлайн (bodyUpdated/bodyHash) обновлён;
//   wake_fresh       — инцидентная геометрия 09.11: тишина от активности
//                      507 мин при свежем lastDevPromptAt (1 мин) — кика нет,
//                      gh-проба не гоняется (ленивость на stallClock);
//   wake_dead        — будка умерла (последний промпт 400 мин назад), PR
//                      открыт: кик — Needs attention + коммент, раз за эпизод;
//   merged_cell      — PR смержен (клетка старого text-формата — путь
//                      prProbeByCell на досках с text-колонкой): кэш
//                      mergedPrUrl = URL клетки, кика нет;
//   merged_cell_url  — PR смержен (клетка url-формата живой доски, после
//                      фикса адаптер читает url — путь prProbeByCell):
//                      кэш = URL клетки, кика нет;
//   unknown_defer    — gh лежит: тики 1–2 кик отложен (полоса unknown),
//                      тик 3 — деградация к not-merged, кик идёт;
//   branch_fallback  — PR-клетка физически пуста (откат клетки ядром —
//                      инцидент 0m2net0, вариант A), merged-PR найден по
//                      ветке: кэш = URL из gh pr list, кика нет.
// HOME/UWUWU_FACTORY_STATE_DIR задаются родителем ДО старта процесса
// (os.homedir() в bun читает env только на старте — инцидент 06.11).
// fetch скриптован (ядро/REST), gh — фейк в PATH (GH_FAKE, журнал GH_LOG).
import { mkdirSync, readFileSync, existsSync } from "node:fs"
import { join } from "node:path"

const SCENARIO = process.env.STALL_GATE_SCENARIO ?? ""

const AV_ID = "20260101000000-abcdef"
const ROW_ID = "20261109000000-row1"
const DOC_ID = "20261109000000-doc1"
const DEV = "ses_dev1"
const NOW = Date.now()

interface SceneCfg {
  status: string
  msgAgeMin: number
  prCell: string
  prCellText?: boolean
  ticks: number
  bodyEdits: number[]
}

const SCENES: Record<string, SceneCfg> = {
  review_swallow: { status: "Review", msgAgeMin: 5, prCell: "", ticks: 3, bodyEdits: [1, 3] },
  wake_fresh: { status: "In Progress", msgAgeMin: 507, prCell: "https://github.com/r/x/pull/42", ticks: 2, bodyEdits: [] },
  wake_dead: { status: "In Progress", msgAgeMin: 500, prCell: "https://github.com/r/x/pull/42", ticks: 2, bodyEdits: [] },
  merged_cell: { status: "In Progress", msgAgeMin: 500, prCell: "https://github.com/r/x/pull/42", prCellText: true, ticks: 1, bodyEdits: [] },
  merged_cell_url: { status: "In Progress", msgAgeMin: 500, prCell: "https://github.com/r/x/pull/42", ticks: 1, bodyEdits: [] },
  unknown_defer: { status: "In Progress", msgAgeMin: 500, prCell: "https://github.com/r/x/pull/42", ticks: 3, bodyEdits: [] },
  branch_fallback: { status: "In Progress", msgAgeMin: 500, prCell: "", ticks: 1, bodyEdits: [] },
}
const cfg = SCENES[SCENARIO]
if (cfg === undefined) throw new Error(`child: неизвестный сценарий ${SCENARIO}`)

// lastmod тела: до первой «правки юзера» — старый (не свежее базлайна state),
// каждая правка двигает вперёд (модель «юзер снова правит тело»)
let docVersion = 0
const docUpdated = (): string => docVersion === 0 ? "20261105120000" : String(20261110120000 + docVersion)
const docContent = (): string => `# Задача\n\nтело задачи v${docVersion}\n\n## Комменты\n\n- отчёт держателя\n`

const statusCell = { v: cfg.status }

function scriptTick(): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const method = init?.method ?? "GET"
    if (url.includes("/api/block/appendBlock")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { parentID?: string; data?: string }
      calls.push(`comment:${payload.parentID}:${(payload.data ?? "").slice(0, 200)}`)
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/api/export/exportMdContent")) {
      calls.push("export")
      return new Response(JSON.stringify({
        code: 0, msg: "", data: { content: docContent(), path: "/Задачи x/Задача" },
      }), { status: 200 })
    }
    if (url.includes("/api/notebook/lsNotebooks")) {
      return new Response(JSON.stringify({ code: 0, msg: "", data: { notebooks: [] } }), { status: 200 })
    }
    if (url.includes("/api/query/sql")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { stmt?: string }
      const stmt = payload.stmt ?? ""
      calls.push("sql")
      if (stmt.includes("content = 'Задачи x'")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ id: "board1" }] }), { status: 200 })
      }
      if (stmt.includes("root_id='board1'")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ id: "avblk1", type: "av" }] }), { status: 200 })
      }
      if (stmt.includes("markdown from blocks")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] }), { status: 200 })
      }
      if (stmt.includes("type='d' and id=")) {
        return new Response(JSON.stringify({
          code: 0, msg: "", data: [{ id: DOC_ID, content: "Задача", updated: docUpdated(), box: "20260101000000-box", hpath: "/Задачи x" }],
        }), { status: 200 })
      }
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/mcp")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        method?: string
        params?: { name?: string; arguments?: Record<string, unknown> }
      }
      if (payload.method === "initialize") {
        return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "sid" } })
      }
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      const tool = payload.params?.name ?? "?"
      const args = payload.params?.arguments ?? {}
      let result: unknown
      if (tool === "find_doc_by_title") {
        result = { structuredContent: { matches: [{ id: "board1", inTrash: false }] } }
      } else if (tool === "read_doc") {
        result = { structuredContent: { blocks: [{ id: "avblk1", flavour: "platform:database" }] } }
      } else if (tool === "database") {
        const action = String(args.action ?? "")
        if (action === "render") {
          const page = Number(args.page ?? 1)
          // PR-клетка: prCellText — text-формат (старые доски), иначе
          // url-формат живой доски {"url":{"content":...}}; после фикса
          // адаптера (диспетч по активному type) url-клетка читается —
          // prProbeByCell достижим; prCell "" — физически пустая клетка
          // (откат клетки ядром) — путь уходит в фоллбек по ветке
          const prValue = cfg.prCell
          result = page === 1
            ? {
                structuredContent: {
                  data: {
                    view: {
                      columns: [
                        { id: "k-status", name: "Status" },
                        { id: "k-pr", name: "PR" },
                        { id: "k-branch", name: "Branch" },
                        { id: "k-dev", name: "Сессия dev" },
                      ],
                      rowCount: 1,
                      rows: [{
                        id: ROW_ID,
                        cells: [
                          {
                            value: {
                              keyID: "k-key", type: "block", blockID: ROW_ID,
                              block: { id: DOC_ID, content: "Задача", refSubtype: "d" },
                            },
                          },
                          { value: { keyID: "k-status", type: "select", mSelect: [{ content: statusCell.v }] } },
                          { value: { keyID: "k-pr", type: "text", text: { content: prValue } } },
                          { value: { keyID: "k-branch", type: "text", text: { content: "work/task-x" } } },
                          { value: { keyID: "k-dev", type: "text", text: { content: DEV } } },
                        ],
                      }],
                    },
                  },
                },
              }
            : { structuredContent: { data: { view: { rows: [] } } } }
        } else if (action === "keys") {
          result = {
            structuredContent: {
              data: { keys: [
                { id: "k-status", name: "Status" }, { id: "k-pr", name: "PR" },
                { id: "k-branch", name: "Branch" }, { id: "k-dev", name: "Сессия dev" },
              ] },
            },
          }
        } else if (action === "item_update") {
          const value = (args.value ?? {}) as { text?: { content?: string }; mSelect?: { content?: string }[] }
          const content = value.text?.content ?? value.mSelect?.map((o) => o.content ?? "").join(",") ?? "?"
          calls.push(`item_update:${String(args.keyID)}=${content}`)
          // доска живая: сигнал фабрики реально меняет Status-ячейку
          if (String(args.keyID) === "k-status") statusCell.v = content
          result = { structuredContent: { data: {} } }
        } else {
          throw new Error(`child mock: неожиданный action database ${action}`)
        }
      } else {
        throw new Error(`child mock: неожиданный тул ${tool}`)
      }
      calls.push(`mcp:${tool}`)
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), { status: 200 })
    }
    if (url.includes("/prompt")) {
      calls.push("prompt")
      return new Response(JSON.stringify({}), { status: 200 })
    }
    if (url.includes("/api/session/")) {
      if (method === "DELETE") {
        calls.push("session-delete")
        return new Response(JSON.stringify({}), { status: 200 })
      }
      if (url.includes("/message")) {
        calls.push("session-message")
        const age = cfg.msgAgeMin * 60_000
        return new Response(JSON.stringify([{ time: { created: NOW - age, streamed: NOW - age } }]), { status: 200 })
      }
      calls.push("session-get")
      return new Response(JSON.stringify({ id: DEV }), { status: 200 })
    }
    if (url.includes("/api/session")) {
      calls.push("session-create")
      return new Response(JSON.stringify({ id: "ses_new" }), { status: 200 })
    }
    throw new Error(`child mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return calls
}

const home = process.env.HOME ?? ""
if (home === "") throw new Error("child: HOME не задан родителем")
mkdirSync(join(home, ".local/share/opencode/log"), { recursive: true })
const stateFile = join(home, ".local/state/uwuwu-factory/state.json")
const logFile = join(home, ".local/state/uwuwu-factory/uwuwu-factory.log")
const state = JSON.parse(readFileSync(stateFile, "utf-8"))

const calls = scriptTick()
const { tickProject } = await import("../factory/factory")
const boardCfg = { board: "Задачи x", repo: "r/x", local: "/tmp/x" }
const log0 = existsSync(logFile) ? readFileSync(logFile, "utf-8").split("\n").length : 0

const perTick: Array<{ prompts: number; statusUpdates: string[]; comments: string[] }> = []
for (let i = 0; i < cfg.ticks; i++) {
  if (cfg.bodyEdits.includes(i + 1)) docVersion++
  const cut = calls.length
  await tickProject(state, "t", boardCfg)
  const tail = calls.slice(cut)
  perTick.push({
    prompts: tail.filter((c) => c === "prompt").length,
    statusUpdates: tail.filter((c) => c.startsWith("item_update:")).map((x) => x.slice("item_update:".length)),
    comments: tail.filter((c) => c.startsWith("comment:")).map((x) => x.slice("comment:".length)),
  })
}

const logLines = existsSync(logFile) ? readFileSync(logFile, "utf-8").split("\n").slice(log0).filter((l) => l.trim() !== "") : []
const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, {
    mergedPrUrl?: string; prProbeUnknownStreak?: number; stallSignaled?: boolean
    bodyHash?: string; bodyUpdated?: string; lastStatus?: string
  }>
}
const t = after.tasks[ROW_ID] ?? {}
const ghLog = process.env.GH_LOG
const ghCalls = ghLog !== undefined && existsSync(ghLog)
  ? readFileSync(ghLog, "utf-8").split("\n").filter((l) => l.trim() !== "")
  : null
console.log(JSON.stringify({
  scenario: SCENARIO,
  perTick,
  ghCalls,
  state: {
    mergedPrUrl: t.mergedPrUrl ?? null,
    prProbeUnknownStreak: t.prProbeUnknownStreak ?? null,
    stallSignaled: t.stallSignaled ?? null,
    bodyHash: t.bodyHash ?? null,
    bodyUpdated: t.bodyUpdated ?? null,
    lastStatus: t.lastStatus ?? null,
  },
  boardStatus: statusCell.v,
  log: logLines.map((l) => l.replace(/^\S+ /, "")),
}))
