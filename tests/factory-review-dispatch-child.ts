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
//   dev-restart-preserves — строка в «In Progress» (возврат из «Ревью
//                   результата»), девелопер мёртв: рестарт девелопера через
//                   preserveAgentSessions сохраняет живую ревью-сессию,
//                   её счётчик попыток и вид (инвариант 1 задача = 1 держатель
//                   вида; рестарт дева не переспавнивает ревьювера).
// HOME задаётся родителем ДО старта процесса: os.homedir() в bun читает env
// только на старте (инцидент 06.11) — изоляция только через новый процесс.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const SCENARIOS = ["plan-spawn", "plan-to-result", "result-rerun", "legacy-migrate", "dev-restart-preserves"] as const
const envScenario = process.env.REVIEW_DISPATCH_SCENARIO
const SCENARIO = SCENARIOS.find((s) => s === envScenario) ?? "plan-spawn"

const AV_ID = "20260101000000-abcdef"
const ROW_ID = "20261108000000-row1"
const DOC_ID = "20261108000000-doc1"
const NEW_SESSION = "ses_new"

const ROW = {
  id: ROW_ID,
  status: SCENARIO === "plan-spawn"
    ? "Ревью плана"
    : SCENARIO === "dev-restart-preserves" ? "In Progress" : "Ревью результата",
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
// dev-restart-preserves: спавн девелопера проходит env-механику (ensureEnv по
// cfg.local) — нужен реальный git-чекоут с локальным ref origin/master
// (fetch best-effort упадёт на фиктивном remote — env.ts продолжает на
// локальных рефах). Чекоут живёт в HOME-песочнице (см. factory-guard-child).
const localCheckout = join(home, "main-checkout")
if (SCENARIO === "dev-restart-preserves") {
  mkdirSync(localCheckout, { recursive: true })
  const run = (args: string[]): void => {
    Bun.spawnSync(args, { cwd: localCheckout, stdout: "ignore", stderr: "ignore" })
  }
  run(["git", "init", "-b", "master", localCheckout])
  writeFileSync(join(localCheckout, "README.md"), "dispatch-child probe\n")
  run(["git", "-C", localCheckout, "add", "."])
  run(["git", "-C", localCheckout, "-c", "user.email=probe@local", "-c", "user.name=probe", "commit", "-m", "init"])
  run(["git", "-C", localCheckout, "update-ref", "refs/remotes/origin/master", "HEAD"])
}
const cfg = { board: "Задачи x", repo: "r/x", local: SCENARIO === "dev-restart-preserves" ? localCheckout : "/tmp/x" }
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
  sessionID?: string
  reviewSessionID?: string
  reviewKind?: string
  reviewAttempts?: number
  lastStatus?: string
}>)[ROW_ID]

// Регресс-пин класса PR #207 (строгий pathname-матч): двойной /api-префикс —
// битый прод-путь (живой сервер даёт 404), мок не имеет права его принимать;
// откат матчинга на суффикс/подстроку делает пин ложным → мастер-тест красный
const strictPathPin = await fetch("http://mock.invalid/api/api/session", { method: "POST", body: "{}" })
  .then(() => false, () => true)

const verdict = {
  scenario: SCENARIO,
  tick1,
  strictPathPin,
  devSessionID: tracked?.sessionID ?? null,
  reviewSessionID: tracked?.reviewSessionID ?? null,
  reviewKind: tracked?.reviewKind ?? null,
  reviewAttempts: tracked?.reviewAttempts ?? null,
  lastStatus: tracked?.lastStatus ?? null,
}
console.log(JSON.stringify(verdict))
