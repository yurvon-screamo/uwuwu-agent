// factory-review-dispatch-child.ts — спавн-ребёнок фиксационного теста
// диспетчеризации ревьюверов. Сценарии — env REVIEW_DISPATCH_SCENARIO:
//   plan-spawn    — строка в «Ревью плана», ревью-сессии нет: спавн агента
//                   plan-reviewer, state запоминает reviewKind="plan",
//                   ячейка «Сессия ревью» перезаписана;
//   plan-to-result— строка в «Ревью результата», живая план-сессия
//                   (reviewKind="plan"): спавн НОВОГО держателя
//                   code-quality-reviewer, не дельта;
//   result-rerun  — строка в «Ревью результата», lastStatus «In Progress»,
//                   живая result-сессия: дельта-промпт в ту же сессию,
//                   без создания новой;
//   legacy-migrate— строка в «Ревью результата», живая сессия без reviewKind
//                   (state старого деплоя): переспавн нового держателя.
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11) — изоляция только через новый процесс.
import { mkdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

const SCENARIOS = ["plan-spawn", "plan-to-result", "result-rerun", "legacy-migrate"] as const
const envScenario = process.env.REVIEW_DISPATCH_SCENARIO
const SCENARIO = SCENARIOS.find((s) => s === envScenario) ?? "plan-spawn"

const AV_ID = "20260101000000-abcdef"
const ROW_ID = "20261108000000-row1"
const DOC_ID = "20261108000000-doc1"
const NEW_SESSION = "ses_new"

const ROW = {
  id: ROW_ID,
  status: SCENARIO === "plan-spawn" ? "Ревью плана" : "Ревью результата",
  bodyDoc: DOC_ID,
  title: "Диспетчер ревью",
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
        data: { content: `# ${ROW.title}\n\nтело задачи\n`, path: "/Задачи x/Диспетчер ревью" },
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
        agent?: string; title?: string; model?: { id?: string }
      }
      calls.push(`session-create:${payload.agent ?? "default"}:${payload.title ?? ""}:${payload.model?.id ?? ""}`)
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
const { tickProject } = await import("../factory/factory")
const cfg = { board: "Задачи x", repo: "r/x", local: "/tmp/x" }
const sliceAll = (list: string[], prefix: string): string[] =>
  list.filter((c) => c.startsWith(prefix)).map((c) => c.slice(prefix.length))
// tick 1
await tickProject(state, "t", cfg)
const tick1 = {
  sessionCreates: sliceAll(calls, "session-create:"),
  prompts: sliceAll(calls, "prompt:"),
  itemUpdates: sliceAll(calls, "item_update:"),
}
const tracked = (state.tasks as Record<string, {
  reviewSessionID?: string
  reviewKind?: string
  reviewAttempts?: number
  lastStatus?: string
}>)[ROW_ID]
const verdict = {
  scenario: SCENARIO,
  tick1,
  reviewSessionID: tracked?.reviewSessionID ?? null,
  reviewKind: tracked?.reviewKind ?? null,
  reviewAttempts: tracked?.reviewAttempts ?? null,
  lastStatus: tracked?.lastStatus ?? null,
}
console.log(JSON.stringify(verdict))
