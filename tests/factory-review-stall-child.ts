// factory-review-stall-child.ts — спавн-ребёнок для тик-теста гварда
// ревью-тишины фабрики (баг «вердикт и статус не применяются при успешной
// сессии ревьювера», задача 20261107142321). Сценарий — env REVIEW_STALL_SCENARIO:
//   stall     — трек в «Ревью результата» с живой ревью-сессией, ход завершился
//               давно (тишина > порога), статус из ревью не ушёл: ожидание —
//               сигнал юзеру + Needs attention, раз за эпизод (два тика подряд —
//               второй без повторного сигнала);
//   healthy   — та же геометрия, активность ревьювера свежая: гвард молчит;
//   recovered — активность вернулась при стоявшем флаге эпизода: флаг снят
//               без сигнала (эпизод завершён);
//   dead      — ревью-сессия удалена (404): штатный spawn-reviewer routeTask,
//               гвард не вмешивается.
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11) — изоляция только через новый процесс.
import { mkdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

const SCENARIO = process.env.REVIEW_STALL_SCENARIO === "healthy"
  ? "healthy"
  : process.env.REVIEW_STALL_SCENARIO === "recovered"
    ? "recovered"
    : process.env.REVIEW_STALL_SCENARIO === "dead" ? "dead" : "stall"

const AV_ID = "20260101000000-abcdef"
const ROW_ID = "20261107000000-row1"
const DOC_ID = "20261107000000-doc1"
const REVIEW_SESSION = SCENARIO === "dead" ? "ses_dead" : "ses_rev1"
/** давность последнего сообщения ревью-сессии: stall — за порогом (мин+1),
 *  healthy/recovered — минуту назад (ход живой или вернулся) */
const MESSAGE_AGE_MS = SCENARIO === "stall" ? 31 * 60_000 : 60_000
const NOW = Date.now()

const ROW = {
  id: ROW_ID,
  status: "Ревью результата",
  reviewSession: REVIEW_SESSION,
  bodyDoc: DOC_ID,
  title: "Зависшее ревью",
}

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
    if (url.includes("/api/export/exportMdContent")) {
      calls.push("export")
      return new Response(JSON.stringify({
        code: 0, msg: "",
        data: { content: `# ${ROW.title}\n\nтело задачи\n`, path: "/Задачи x/Зависшее ревью" },
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
                          { value: { keyID: "k-rev", type: "text", text: { content: ROW.reviewSession } } },
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
              data: { keys: [{ id: "k-status", name: "Status" }, { id: "k-dev", name: "Сессия dev" }, { id: "k-rev", name: "Сессия ревью" }] },
            },
          }
        } else if (action === "item_update") {
          const value = (args.value ?? {}) as { text?: { content?: string }; mSelect?: { content?: string }[] }
          const content = value.text?.content ?? value.mSelect?.map((o) => o.content ?? "").join(",") ?? "?"
          calls.push(`item_update:${String(args.keyID)}=${content}`)
          // доска живая: сигнал фабрики реально меняет Status-ячейку —
          // эмулируем, иначе второй тик видит устаревший статус
          if (String(args.keyID) === "k-status") ROW.status = content
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
      // активность ревью-сессии: /message — массив [{ time: { created, … } }]
      if (url.includes("/message")) {
        calls.push("session-message")
        return new Response(JSON.stringify([{ time: { created: NOW - MESSAGE_AGE_MS, streamed: NOW - MESSAGE_AGE_MS } }]), { status: 200 })
      }
      calls.push("session-get")
      const sid = url.split("/api/session/")[1]?.split(/[/?]/)[0] ?? ""
      if (sid === "ses_dead") {
        return new Response(JSON.stringify({ code: -1, msg: "session gone" }), { status: 404 })
      }
      return new Response(JSON.stringify({ id: sid || "ses_x" }), { status: 200 })
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
const home = process.env.HOME ?? ""
if (home === "") throw new Error("child: HOME не задан родителем")
mkdirSync(join(home, ".local/share/opencode/log"), { recursive: true })
const stateFile = join(home, ".local/state/uwuwu-factory/state.json")
const state = JSON.parse(readFileSync(stateFile, "utf-8"))

const calls = scriptTick()
const { tickProject } = await import("../factory/factory")
const cfg = { board: "Задачи x", repo: "r/x", local: "/tmp/x" }
// tick 1
await tickProject(state, "t", cfg)
const cut = calls.length
const tick1 = {
  comments: calls.filter((c) => c.startsWith("comment:")).map((x) => x.slice("comment:".length)),
  itemUpdates: calls.filter((c) => c.startsWith("item_update:")).map((x) => x.slice("item_update:".length)),
  prompts: calls.filter((c) => c === "prompt").length,
  sessionCreates: calls.filter((c) => c === "session-create").length,
}
// tick 2 (анти-спам / повторная детекция)
await tickProject(state, "t", cfg)
const tail = calls.slice(cut)
const tick2 = {
  comments: tail.filter((c) => c.startsWith("comment:")).map((x) => x.slice("comment:".length)),
  itemUpdates: tail.filter((c) => c.startsWith("item_update:")).map((x) => x.slice("item_update:".length)),
  prompts: tail.filter((c) => c === "prompt").length,
  sessionCreates: tail.filter((c) => c === "session-create").length,
}

const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, { reviewStallSignaled?: boolean; lastStatus?: string; reviewSessionID?: string }>
}
const track = after.tasks[ROW_ID] ?? {}
console.log(JSON.stringify({
  scenario: SCENARIO,
  tick1, tick2,
  reviewStallSignaled: track.reviewStallSignaled ?? null,
  lastStatus: track.lastStatus ?? "",
  reviewSessionID: track.reviewSessionID ?? "",
}))
