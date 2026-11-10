// factory-testing-stage-child.ts — спавн-ребёнок для тик-теста стадии
// «Тестирование» фабрики. Сценарии — env TESTING_STAGE_SCENARIO:
//   quarantine — untracked строка в «Тестировании» (даунтайм демона на
//               переходе): карантинная запись state + спавн тестировщика
//               (агент tester, cwd ворктри, ячейка «Сессия тест»), дев-волна
//               НЕ спавнится; повторный тик не плодит дубль-спавн;
//   nocell     — доска без колонки «Сессия тест»: спавн не пишет ячейку,
//               громкий флаг [factory] доставлен раз на доску (дедуп
//               держит рестарт-спавн tick3);
//   rerun      — трек с живой тест-сессией, lastStatus «Ревью результата»:
//               дельта-промпт в ТУ ЖЕ сессию, без создания новой;
//   bug        — fail-вердикт тестировщика: строка в In Progress, дев-сессия
//               жива — возврат девелоперу промптом, тест-сессия пережита.
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11) — изоляция только через новый процесс.
import { mkdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

const RAW_SCENARIO = process.env.TESTING_STAGE_SCENARIO ?? ""
const SCENARIO = RAW_SCENARIO === "rerun" || RAW_SCENARIO === "bug" || RAW_SCENARIO === "nocell"
  ? RAW_SCENARIO
  : "quarantine"

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

// Схема доски (render columns + keys): на nocell «Сессия тест» отсутствует —
// чтение cells и запись ячеек видят одинаковую раскладку
const BOARD_KEYS = [
  { id: "k-status", name: "Status" },
  { id: "k-dev", name: "Сессия dev" },
  { id: "k-rev", name: "Сессия ревью" },
  { id: "k-test", name: "Сессия тест" },
]
const viewKeys = SCENARIO === "nocell" ? BOARD_KEYS.filter((k) => k.id !== "k-test") : BOARD_KEYS

function scriptTick(): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const method = init?.method ?? "GET"
    if (url.includes("/api/block/appendBlock")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { data?: string }
      calls.push(`comment:${payload.data ?? ""}`)
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
                      columns: viewKeys,
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
          result = { structuredContent: { data: { keys: viewKeys } } }
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
    // opencode REST — строгий pathname-матч (класс PR #207): rest демона строит
    // `${base}/api${path}`, двойной префикс (/api/api/…) на живом сервере даёт
    // 404 — мок не имеет права его принимать; незнакомый путь = громкий отказ
    const path = new URL(url).pathname
    if (/^\/api\/session\/(ses_[^/]+)\/prompt$/.test(path)) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { text?: string }
      calls.push(`prompt:${(payload.text ?? "").slice(0, 800)}`)
      return new Response(JSON.stringify({}), { status: 200 })
    }
    const sessMatch = /^\/api\/session\/(ses_[^/]+)(\/message)?$/.exec(path)
    if (sessMatch !== null) {
      if (method === "DELETE") {
        calls.push("session-delete")
        return new Response(JSON.stringify({}), { status: 200 })
      }
      if (sessMatch[2] === "/message") {
        calls.push("session-message")
        return new Response(JSON.stringify([{ time: { created: Date.now() - 60_000, streamed: Date.now() - 60_000 } }]), { status: 200 })
      }
      calls.push("session-get")
      return new Response(JSON.stringify({ id: sessMatch[1] || "ses_x" }), { status: 200 })
    }
    if (path === "/api/session") {
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
const cut = calls.length
const tick1 = {
  sessionCreates: sliceAll(calls, "session-create:"),
  prompts: sliceAll(calls, "prompt:"),
  itemUpdates: sliceAll(calls, "item_update:"),
  comments: sliceAll(calls, "comment:"),
}
// tick 2 (дубль-спавн / повторная доставка)
await tickProject(state, "t", cfg)
const tail = calls.slice(cut)
const tick2 = {
  sessionCreates: sliceAll(tail, "session-create:"),
  prompts: sliceAll(tail, "prompt:"),
  itemUpdates: sliceAll(tail, "item_update:"),
  comments: sliceAll(tail, "comment:"),
}
// tick 3 — рестарт-спавн (только nocell): трек тест-сессии потерян
// (даунтайм демона), строка снова untracked — спавн повторяется, а
// флаг-коммент НЕ доставляется второй раз (дедуп на доску)
let tick3 = { sessionCreates: [] as string[], itemUpdates: [] as string[], comments: [] as string[] }
if (SCENARIO === "nocell") {
  delete state.tasks[ROW_ID]
  const cut3 = calls.length
  await tickProject(state, "t", cfg)
  const tail3 = calls.slice(cut3)
  tick3 = {
    sessionCreates: sliceAll(tail3, "session-create:"),
    itemUpdates: sliceAll(tail3, "item_update:"),
    comments: sliceAll(tail3, "comment:"),
  }
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

// Регресс-пин класса PR #207 (строгий pathname-матч): двойной /api-префикс —
// битый прод-путь (живой сервер даёт 404), мок не имеет права его принимать;
// пин ловит полный откат класса и откат протыкаемой ветки; точечный откат
// прочих веток добивают ассерты вердиктов мастер-тестов
const strictPathPin = await fetch("http://mock.invalid/api/api/session", { method: "POST", body: "{}" })
  .then(() => false, () => true)

console.log(JSON.stringify({
  scenario: SCENARIO,
  tick1,
  tick2,
  tick3,
  strictPathPin,
  sessionID: track.sessionID ?? null,
  testSessionID: track.testSessionID ?? null,
  testAttempts: track.testAttempts ?? null,
  reviewSessionID: track.reviewSessionID ?? null,
  lastStatus: track.lastStatus ?? null,
}))
