// factory-tick-child.ts — спавн-ребёнок для тик-теста фабрики (не запускать
// руками). HOME/UWUWU_SIYUAN_TOKEN задаются родителем ДО старта процесса:
// os.homedir() в bun читает env только на старте — runtime-подмена не
// двигает STATE_DIR/SERVICE_REG (инцидент 06.11: тест ходил в прод-state).
// Ребёнок скриптует fetch (ядро/REST), грузит factory/factory.ts, гонит тик
// и печатает вердикт JSON в stdout — родитель сверяет.
import { mkdirSync, mkdtempSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const ROW_ID = "20261105000000-row1"
const BODY_DOC = "20261105000000-bodydoc"
const SESSION_ID = "ses_livetest0000000000000000000"

function scriptTick(): string[] {
  const calls: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
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
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: '<div data-av-id="20260101000000-abcdef"></div>' }] }), { status: 200 })
      }
      if (stmt.includes("type='d' and id=")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ id: BODY_DOC, content: "Тестовая задача", updated: "20261105120000" }] }), { status: 200 })
      }
      return new Response(JSON.stringify({ code: 0, msg: "", data: [] }), { status: 200 })
    }
    if (url.includes("/api/export/exportMdContent")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as { id?: string }
      calls.push(`exportMd:${payload.id}`)
      return new Response(JSON.stringify({ code: 0, msg: "", data: { content: "" } }), { status: 200 })
    }
    // opencode REST — строгий pathname-матч (класс PR #207): rest демона строит
    // `${base}/api${path}`, двойной префикс (/api/api/…) на живом сервере даёт
    // 404 — мок не имеет права его принимать; незнакомый путь = громкий отказ
    const path = new URL(url).pathname
    const sessMatch = /^\/api\/session\/(ses_[^/]+)(\/(message|prompt))?$/.exec(path)
    if (sessMatch !== null) {
      calls.push("rest-session")
      return new Response(JSON.stringify({ id: SESSION_ID }), { status: 200 })
    }
    if (path === "/api/session") {
      calls.push("rest-prompt")
      return new Response(JSON.stringify({}), { status: 200 })
    }
    if (url.includes("/mcp")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        method?: string
        params?: { name?: string; arguments?: { action?: string; page?: number } }
      }
      if (payload.method === "initialize") {
        return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: {} }), { status: 200, headers: { "mcp-session-id": "sid" } })
      }
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      const tool = payload.params?.name ?? "?"
      let result: unknown
      if (tool === "find_doc_by_title") {
        result = { structuredContent: { matches: [{ id: "board1", inTrash: false }] } }
      } else if (tool === "read_doc") {
        result = { structuredContent: { blocks: [{ id: "avblk1", flavour: "platform:database" }] } }
      } else if (tool === "database") {
        result = payload.params?.arguments?.page === 1
          ? {
              structuredContent: {
                data: {
                  view: {
                    columns: [{ id: "k-status", name: "Status" }],
                    rowCount: 1,
                    rows: [{
                      id: ROW_ID,
                      cells: [
                        {
                          value: {
                            keyID: "k-key", type: "block", blockID: ROW_ID,
                            block: { id: BODY_DOC, content: "Тестовая задача", refSubtype: "d" },
                          },
                        },
                        { value: { keyID: "k-status", type: "select", mSelect: [{ content: "In Progress" }] } },
                      ],
                    }],
                  },
                },
              },
            }
          : { structuredContent: { data: { view: { rows: [] } } } }
      } else {
        throw new Error(`child mock: неожиданный тул ${tool}`)
      }
      calls.push(`mcp:${tool}`)
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), { status: 200 })
    }
    throw new Error(`child mock: неожиданный путь ${url}`)
  }) as typeof fetch
  return calls
}

// ——— песочница: HOME от родителя, state/service уже на месте ———
const home = process.env.HOME ?? mkdtempSync(join(tmpdir(), "factory-child-"))
mkdirSync(join(home, ".local/share/opencode/log"), { recursive: true })
const stateFile = join(home, ".local/state/uwuwu-factory/state.json")
const state = JSON.parse(readFileSync(stateFile, "utf-8"))

const calls = scriptTick()
const { tickProject } = await import("../factory/factory")
await tickProject(state, "t", { board: "Задачи x", repo: "r/x", local: "/tmp/x" })

const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, { bodyDoc: string; bodyUpdated?: string }>
}

// Регресс-пин класса PR #207 (строгий pathname-матч): двойной /api-префикс —
// битый прод-путь (живой сервер даёт 404), мок не имеет права его принимать;
// пин ловит полный откат класса и откат протыкаемой ветки; точечный откат
// прочих веток добивают ассерты вердиктов мастер-тестов
const strictPathPin = await fetch("http://mock.invalid/api/api/session", { method: "POST", body: "{}" })
  .then(() => false, () => true)

console.log(JSON.stringify({
  bodyDoc: after.tasks[ROW_ID]?.bodyDoc ?? null,
  bodyUpdated: after.tasks[ROW_ID]?.bodyUpdated ?? null,
  exportIds: calls.filter((c) => c.startsWith("exportMd:")).map((c) => c.slice("exportMd:".length)),
  prompts: calls.filter((c) => c === "rest-prompt").length,
  strictPathPin,
}))
