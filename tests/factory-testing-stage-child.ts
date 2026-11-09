// factory-testing-stage-child.ts — спавн-ребёнок для тик-теста стадии
// «Тестирование» фабрики. Сценарии — env TESTING_STAGE_SCENARIO:
//   quarantine — untracked строка в «Тестировании» (даунтайм демона на
//               переходе): карантинная запись state + спавн тестировщика
//               (агент tester, cwd ворктри, ячейка «Сессия тест»), дев-волна
//               НЕ спавнится; повторный тик не плодит дубль-спавн;
//   rerun      — трек с живой тест-сессией, lastStatus «Ревью результата»:
//               дельта-промпт в ТУ ЖЕ сессию, без создания новой;
//   bug        — fail-вердикт тестировщика: строка в In Progress, дев-сессия
//               жива — возврат девелоперу промптом, тест-сессия пережита.
//   worktree   — как quarantine, но в песочном ENV_ROOT посажен живой
//               агентский ворктри env:false (~/agents-env/<rowId>/repo c .git):
//               резолв (20261109133634-6ctgzew) должен дать тестировщику
//               живой путь — в cwd сессии и в промпте, без фоллбэк-строки.
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11) — изоляция только через новый процесс.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const SCENARIO = process.env.TESTING_STAGE_SCENARIO === "rerun"
  ? "rerun"
  : process.env.TESTING_STAGE_SCENARIO === "bug"
  ? "bug"
  : process.env.TESTING_STAGE_SCENARIO === "worktree" ? "worktree" : "quarantine"

const AV_ID = "20260101000000-abcdef"
const ROW_ID = "20261108000000-row1"
const DOC_ID = "20261108000000-doc1"
const NEW_SESSION = "ses_new"

const ROW = {
  id: ROW_ID,
  status: SCENARIO === "bug" ? "In Progress" : "Тестирование",
  bodyDoc: DOC_ID,
  title: "Тестовая стадия",
}

function scriptTick(): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const method = init?.method ?? "GET"
    if (url.includes("/api/block/appendBlock")) {
      calls.push("comment")
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/api/export/exportMdContent")) {
      calls.push("export")
      return new Response(JSON.stringify({
        code: 0, msg: "",
        data: { content: `# ${ROW.title}\n\nтело задачи\n`, path: "/Задачи x/Тестовая стадия" },
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
          code: 0, msg: "",
          data: [{ id: DOC_ID, content: ROW.title, updated: "20261106120000", box: "20260101000000-box", hpath: "/Задачи x" }],
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
        return new Response(JSON.stringify({ jsonrpc: 2.0, id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "sid" } })
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
          result = page === 1
            ? {
                structuredContent: {
                  data: {
                    view: {
                      columns: [
                        { id: "k-status", name: "Status" },
                        { id: "k-dev", name: "Сессия dev" },
                        { id: "k-rev", name: "Сессия ревью" },
                        { id: "k-test", name: "Сессия тест" },
                      ],
                      rowCount: 1,
                      rows: [{
                        id: ROW.id,
                        cells: [
                          {
                            value: {
                              keyID: "k-key", type: "block", blockID: ROW.id,
                              block: { id: ROW.bodyDoc, content: ROW.title, refSubtype: "d" },
                            },
                          },
                          { value: { keyID: "k-status", type: "select", mSelect: [{ content: ROW.status }] } },
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
                { id: "k-status", name: "Status" },
                { id: "k-dev", name: "Сессия dev" },
                { id: "k-rev", name: "Сессия ревью" },
                { id: "k-test", name: "Сессия тест" },
              ] },
            },
          }
        } else if (action === "item_update") {
          const value = (args.value ?? {}) as { text?: { content?: string }; mSelect?: { content?: string }[] }
          const content = value.text?.content ?? value.mSelect?.map((o) => o.content ?? "").join(",") ?? "?"
          calls.push(`item_update:${String(args.keyID)}=${content}`)
          result = { structuredContent: { data: {} } }
        } else {
          throw new Error(`child mock: неожиданный action database ${action}`)
        }
      } else {
        throw new Error(`child mock: неожиданный тул ${tool}`)
      }
      calls.push(`mcp:${tool}`)
      return new Response(JSON.stringify({ jsonrpc: 2.0, id: 1, result }), { status: 200 })
    }
    if (url.includes("/prompt")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { text?: string }
      calls.push(`prompt:${(payload.text ?? "").slice(0, 800)}`)
      return new Response(JSON.stringify({}), { status: 200 })
    }
    if (url.includes("/api/session/")) {
      if (method === "DELETE") {
        calls.push("session-delete")
        return new Response(JSON.stringify({}), { status: 200 })
      }
      if (url.includes("/message")) {
        calls.push("session-message")
        return new Response(JSON.stringify([{ time: { created: Date.now() - 60_000, streamed: Date.now() - 60_000 } }]), { status: 200 })
      }
      calls.push("session-get")
      const sid = url.split("/api/session/")[1]?.split(/[/?]/)[0] ?? ""
      return new Response(JSON.stringify({ id: sid || "ses_x" }), { status: 200 })
    }
    if (url.includes("/api/session")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        agent?: string; title?: string; model?: { id?: string }; location?: { directory?: string }
      }
      // directory в хвосте: cwd сессии — ключевой ассерт worktree-сценария
      calls.push(`session-create:${payload.agent ?? "default"}:${payload.title ?? ""}:${payload.model?.id ?? ""}:${payload.location?.directory ?? "?"}`)
      return new Response(JSON.stringify({ id: NEW_SESSION }), { status: 200 })
    }
    throw new Error(`child mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return calls
}

// ——— песочница: HOME от родителя, state/service уже на месте ———
const home = process.env.HOME ?? ""
if (home === "") throw new Error("child: HOME не задан родителем")
mkdirSync(join(home, ".local/share/opencode/log"), { recursive: true })
const stateFile = join(home, ".local/state/uwuwu-factory/state.json")
const state = JSON.parse(readFileSync(stateFile, "utf-8"))

const calls = scriptTick()
// worktree-сценарий: живой агентский ворктри env:false в песочном ENV_ROOT
// (ENV_ROOT = homedir()/agents-env; .git — файл, как у реального worktree)
if (SCENARIO === "worktree") {
  const repoDir = join(home, "agents-env", ROW_ID, "repo")
  mkdirSync(repoDir, { recursive: true })
  writeFileSync(join(repoDir, ".git"), "gitdir: /elsewhere/main/.git/worktrees/x\n")
}
const { tickProject } = await import("../factory/factory")
const cfg = { board: "Задачи x", repo: "r/x", local: "/tmp/x" }
const sliceAll = (list: string[], prefix: string): string[] =>
  list.filter((c) => c.startsWith(prefix)).map((c) => c.slice(prefix.length))
// tick 1
await tickProject(state, "t", cfg)
const cut = calls.length
const tick1 = {
  sessionCreates: sliceAll(calls, "session-create:"),
  prompts: sliceAll(calls, "prompt:"),
  itemUpdates: sliceAll(calls, "item_update:"),
}
// tick 2 (дубль-спавн / повторная доставка)
await tickProject(state, "t", cfg)
const tail = calls.slice(cut)
const tick2 = {
  sessionCreates: sliceAll(tail, "session-create:"),
  prompts: sliceAll(tail, "prompt:"),
  itemUpdates: sliceAll(tail, "item_update:"),
}

const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, {
    sessionID: string
    testSessionID?: string
    testAttempts?: number
    reviewSessionID?: string
    lastStatus?: string
  }>
}
const track = after.tasks[ROW_ID] ?? {}
console.log(JSON.stringify({
  scenario: SCENARIO,
  tick1,
  tick2,
  sessionID: track.sessionID ?? null,
  testSessionID: track.testSessionID ?? null,
  testAttempts: track.testAttempts ?? null,
  reviewSessionID: track.reviewSessionID ?? null,
  lastStatus: track.lastStatus ?? null,
}))
