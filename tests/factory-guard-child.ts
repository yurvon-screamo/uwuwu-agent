// factory-guard-child.ts — спавн-ребёнок для тик-теста гарда каскада фабрики
// (не запускать руками). Сценарий — env GUARD_SCENARIO:
//   hold       — пустой state + доска с сиротами-спавнами (строки с сессионной
//                ячейкой без трека): HOLD, алерт, бэкап, ноль спавнов;
//   resolve    — доска без spawnable-строк: allow при стоящем флаге эпизода —
//                демон обязан снять holdSignaled (выход из эпизода);
//   done-clean — трекнутая Done-строка с сессионными ячейками: чистка state
//                + очистка ячеек (инвариант гарда), ноль спавнов.
// HOME задаются родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11: тест с runtime-HOME ходил в прод-state).
import { mkdirSync, mkdtempSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const SCENARIO = process.env.GUARD_SCENARIO === "done-clean"
  ? "done-clean"
  : process.env.GUARD_SCENARIO === "resolve" ? "resolve" : "hold"
const AV_ID = "20260101000000-abcdef"

interface MockRow {
  id: string
  status: string
  devSession?: string
  reviewSession?: string
  bodyDoc: string
  title: string
}

const HOLD_ROWS: MockRow[] = [1, 2, 3].map((n) => ({
  id: `20261106000000-row${n}`,
  status: "Design",
  devSession: `ses_old${n}`,
  bodyDoc: `20261106000000-doc${n}`,
  title: `Сирота-спавн ${n}`,
}))

const DONE_ROWS: MockRow[] = [{
  id: "20261105000000-row1",
  status: "Done",
  devSession: "ses_old1",
  bodyDoc: "20261105000000-doc1",
  title: "Закрытая задача",
}]

// resolve: доска без spawnable-строк — сирот и untracked нет, allow при любом флаге
const RESOLVE_ROWS: MockRow[] = HOLD_ROWS.map((r) => ({ ...r, status: "Todo", devSession: undefined }))

const rows = SCENARIO === "done-clean" ? DONE_ROWS : SCENARIO === "resolve" ? RESOLVE_ROWS : HOLD_ROWS

function scriptTick(): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const method = init?.method ?? "GET"
    if (url.includes("/api/block/appendBlock")) {
      // create_comment платформенного слоя пишет секцию «## Комменты» ядру напрямую
      const payload = JSON.parse(String(init?.body ?? "{}")) as { parentID?: string }
      calls.push(`comment:${payload.parentID}`)
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
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
        const docId = (stmt.match(/id='([^']+)'/) ?? [])[1] ?? ""
        const row = rows.find((r) => r.bodyDoc === docId)
        return new Response(JSON.stringify({
          code: 0, msg: "",
          data: row !== undefined
            ? [{ id: docId, content: row.title, updated: "20261106120000", box: "20260101000000-box", hpath: "/Задачи x" }]
            : [],
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
                      ],
                      rowCount: rows.length,
                      rows: rows.map((r) => ({
                        id: r.id,
                        cells: [
                          {
                            value: {
                              keyID: "k-key", type: "block", blockID: r.id,
                              block: { id: r.bodyDoc, content: r.title, refSubtype: "d" },
                            },
                          },
                          { value: { keyID: "k-status", type: "select", mSelect: [{ content: r.status }] } },
                          ...(r.devSession !== undefined
                            ? [{ value: { keyID: "k-dev", type: "text", text: { content: r.devSession } } }]
                            : []),
                          ...(r.reviewSession !== undefined
                            ? [{ value: { keyID: "k-rev", type: "text", text: { content: r.reviewSession } } }]
                            : []),
                        ],
                      })),
                    },
                  },
                },
              }
            : { structuredContent: { data: { view: { rows: [] } } } }
        } else if (action === "keys") {
          result = {
            structuredContent: {
              data: { keys: [{ id: "k-status", name: "Status" }, { id: "k-dev", name: "Сессия dev" }, { id: "k-rev", name: "Сессия ревью" }] },
            },
          }
        } else if (action === "item_update") {
          const value = (args.value ?? {}) as { text?: { content?: string } }
          calls.push(`item_update:${String(args.keyID)}=${value.text?.content ?? "?"}`)
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
      calls.push("prompt")
      return new Response(JSON.stringify({}), { status: 200 })
    }
    if (url.includes("/api/session/")) {
      if (method === "DELETE") {
        calls.push("session-delete")
        return new Response(JSON.stringify({}), { status: 200 })
      }
      calls.push("session-get")
      return new Response(JSON.stringify({ id: "ses_x" }), { status: 200 })
    }
    if (url.includes("/api/session")) {
      calls.push("session-create")
      return new Response(JSON.stringify({ id: "ses_new" }), { status: 200 })
    }
    throw new Error(`child mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return calls
}

// ——— песочница: HOME от родителя, state/service уже на месте ———
const home = process.env.HOME ?? mkdtempSync(join(tmpdir(), "factory-guard-child-"))
mkdirSync(join(home, ".local/share/opencode/log"), { recursive: true })
const stateFile = join(home, ".local/state/uwuwu-factory/state.json")
const state = JSON.parse(readFileSync(stateFile, "utf-8"))

const calls = scriptTick()
const { tickProject } = await import("../factory/factory")
await tickProject(state, "t", { board: "Задачи x", repo: "r/x", local: "/tmp/x" })

const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, unknown>
  holdSignaled?: string
}
console.log(JSON.stringify({
  scenario: SCENARIO,
  taskCount: Object.keys(after.tasks).length,
  holdSignaled: after.holdSignaled ?? null,
  sessionCreates: calls.filter((c) => c === "session-create").length,
  sessionDeletes: calls.filter((c) => c === "session-delete").length,
  prompts: calls.filter((c) => c === "prompt").length,
  comments: calls.filter((c) => c.startsWith("comment:")).map((x) => x.slice("comment:".length)),
  itemUpdates: calls.filter((c) => c.startsWith("item_update:")).map((x) => x.slice("item_update:".length)),
}))
