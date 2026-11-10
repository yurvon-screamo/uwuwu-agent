// factory-guard-child.ts — спавн-ребёнок для тик-теста гарда каскада фабрики
// (не запускать руками). Сценарий — env GUARD_SCENARIO:
//   hold       — пустой state + доска с сиротами-спавнами (строки с сессионной
//                ячейкой без трека): HOLD, алерт, бэкап, ноль спавнов;
//   resolve    — доска без spawnable-строк: allow при стоящем флаге эпизода —
//                демон обязан снять holdSignaled (выход из эпизода);
//   done-clean — трекнутая Done-строка с сессионными ячейками: чистка state
//                + очистка ячеек (инвариант гарда), ноль спавнов;
//   claim-skip — пустой state + untracked In Progress с живой дев-заявкой
//                (task claim): внешняя работа идёт — не спавнить дубль,
//                HOLD-эпизода нет (20261105174319);
//   claim-dead — пустой state + untracked In Progress с мёртвой дев-заявкой:
//                сирота как прежде и остаётся в дев-волне — при allow
//                ре-спавн компенсирует (автокомпенсация lossMax-допуска).
// HOME задаются родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11: тест с runtime-HOME ходил в прод-state).
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const SCENARIO = process.env.GUARD_SCENARIO === "done-clean"
  ? "done-clean"
  : process.env.GUARD_SCENARIO === "claim-skip" || process.env.GUARD_SCENARIO === "claim-dead"
    ? process.env.GUARD_SCENARIO
    : process.env.GUARD_SCENARIO === "resolve" ? "resolve" : "hold"
const AV_ID = "20260101000000-abcdef"

interface MockRow {
  id: string
  status: string
  devSession?: string
  reviewSession?: string
  testSession?: string
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

// claim-skip: untracked In Progress с живой дев-заявкой (мок /api/session/:id отвечает 200)
const CLAIM_ROWS: MockRow[] = [{
  id: "20261107000000-row1",
  status: "In Progress",
  devSession: "ses_ext1",
  bodyDoc: "20261107000000-doc1",
  title: "Заявленная задача",
}]

const rows = SCENARIO === "done-clean"
  ? DONE_ROWS
  : SCENARIO === "claim-skip" || SCENARIO === "claim-dead"
    ? CLAIM_ROWS
    : SCENARIO === "resolve" ? RESOLVE_ROWS : HOLD_ROWS

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
      // спавн-путь читает тело задачи (kickoff-промпт/adoptBodyDoc)
      const payload = JSON.parse(String(init?.body ?? "{}")) as { id?: string }
      const row = rows.find((r) => r.bodyDoc === payload.id)
      calls.push("export")
      return new Response(JSON.stringify({
        code: 0, msg: "",
        data: { content: `# ${row?.title ?? "таск"}\n\nтело задачи\n`, path: "/Задачи x/таск" },
      }), { status: 200 })
    }
    if (url.includes("/api/notebook/lsNotebooks")) {
      // инит-блок спавна (fetchSpawnInit): песочница без ноутбуков —
      // персона/доки проекта честно деградируют, спавн продолжается
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
                        { id: "k-test", name: "Сессия тест" },
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
                          ...(r.testSession !== undefined
                            ? [{ value: { keyID: "k-test", type: "text", text: { content: r.testSession } } }]
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
              data: { keys: [
                { id: "k-status", name: "Status" },
                { id: "k-dev", name: "Сессия dev" },
                { id: "k-rev", name: "Сессия ревью" },
                { id: "k-test", name: "Сессия тест" },
              ] },
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
    // opencode REST — строгий pathname-матч (класс PR #207): rest демона строит
    // `${base}/api${path}`, двойной префикс (/api/api/…) на живом сервере даёт
    // 404 — мок не имеет права его принимать; незнакомый путь = громкий отказ
    const path = new URL(url).pathname
    if (/^\/api\/session\/(ses_[^/]+)\/prompt$/.test(path)) {
      calls.push("prompt")
      return new Response(JSON.stringify({}), { status: 200 })
    }
    const sessMatch = /^\/api\/session\/(ses_[^/]+)$/.exec(path)
    if (sessMatch !== null) {
      if (method === "DELETE") {
        calls.push("session-delete")
        return new Response(JSON.stringify({}), { status: 200 })
      }
      calls.push("session-get")
      // Мёртвые id по сценарию: hold — сироты ses_old* (404 → state-loss
      // сигнатура); claim-skip — заявка жива (внешняя работа); claim-dead —
      // заявка ses_ext1 мертва, а свежая фабричная сессия после спавна жива.
      const sid = sessMatch[1] ?? ""
      const deadIds = SCENARIO === "claim-dead"
        ? ["ses_ext1"]
        : SCENARIO === "claim-skip" ? [] : ["ses_old1", "ses_old2", "ses_old3"]
      if (deadIds.includes(sid)) {
        return new Response(JSON.stringify({ code: -1, msg: "session gone" }), { status: 404 })
      }
      return new Response(JSON.stringify({ id: sid || "ses_x" }), { status: 200 })
    }
    if (path === "/api/session") {
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

// claim-dead: спавн проходит env-механику (ensureEnv по cfg.local) — нужен
// реальный git-чекоут с локальным ref origin/master (fetch best-effort упадёт
// на фиктивном remote — env.ts продолжает на локальных рефах). Чекоут живёт
// в HOME-песочнице: /tmp/x переживал бы прогоны с остаточными ветками
// (worktree add -b падает «branch already exists» → карантин вместо спавна).
const localCheckout = join(home, "main-checkout")
if (SCENARIO === "claim-dead") {
  mkdirSync(localCheckout, { recursive: true })
  const run = (args: string[]): void => {
    Bun.spawnSync(args, { cwd: localCheckout, stdout: "ignore", stderr: "ignore" })
  }
  run(["git", "init", "-b", "master", localCheckout])
  writeFileSync(join(localCheckout, "README.md"), "guard-child probe\n")
  run(["git", "-C", localCheckout, "add", "."])
  run(["git", "-C", localCheckout, "-c", "user.email=probe@local", "-c", "user.name=probe", "commit", "-m", "init"])
  run(["git", "-C", localCheckout, "update-ref", "refs/remotes/origin/master", "HEAD"])
}

const calls = scriptTick()
const { tickProject } = await import("../factory/factory")
await tickProject(state, "t", { board: "Задачи x", repo: "r/x", local: SCENARIO === "claim-dead" ? localCheckout : "/tmp/x" })

// очередь сигналов дримеру (UWUWU_DREAMER_QUEUE_DIR от родителя): имя → контент
const queueDir = process.env.UWUWU_DREAMER_QUEUE_DIR ?? ""
const queueSignals: Record<string, string> = {}
try {
  for (const name of readdirSync(queueDir)) {
    if (name.endsWith(".json")) queueSignals[name] = readFileSync(join(queueDir, name), "utf-8")
  }
} catch { /* очередь не создавалась — фабрика не писала сигналы */ }

const after = JSON.parse(readFileSync(stateFile, "utf-8")) as {
  tasks: Record<string, unknown>
  holdSignaled?: string
}

// Регресс-пин класса PR #207 (строгий pathname-матч): двойной /api-префикс —
// битый прод-путь (живой сервер даёт 404), мок не имеет права его принимать;
// откат протыкаемой ветки (create) на суффикс/подстроку делает пин ложным →
// мастер-тест красный
const strictPathPin = await fetch("http://mock.invalid/api/api/session", { method: "POST", body: "{}" })
  .then(() => false, () => true)

console.log(JSON.stringify({
  scenario: SCENARIO,
  taskCount: Object.keys(after.tasks).length,
  holdSignaled: after.holdSignaled ?? null,
  strictPathPin,
  sessionCreates: calls.filter((c) => c === "session-create").length,
  sessionDeletes: calls.filter((c) => c === "session-delete").length,
  prompts: calls.filter((c) => c === "prompt").length,
  comments: calls.filter((c) => c.startsWith("comment:")).map((x) => x.slice("comment:".length)),
  itemUpdates: calls.filter((c) => c.startsWith("item_update:")).map((x) => x.slice("item_update:".length)),
  queueSignals,
}))
