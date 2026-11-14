// tests/dreamer-tick-child.ts — песочница тика uwuwu-dreamer для
// dreamer-tick.test.ts: фейк opencode REST (Bun.serve) + фикстурная БД
// + очередь сигналов фабрики → ONCE-тик демона подпроцессом.
// HOME от старта процесса: os.homedir() в bun игнорирует runtime-подмену env
// (инцидент 06.11) — service.json демона изолируется только новым процессом.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { Database } from "bun:sqlite"

const home = process.env.HOME as string
const scenario = process.env.SCENARIO as string
const rowId = "20261109000000-tcktest1"
const DIRECTORY = "/home/x/uwuwu-agent"
// посессионные сценарии (баг 1077): каталог исходной сессии должен существовать —
// живость-гвард call-site подавил бы location фикстурного /home/x/…
const sessionScenario = scenario.startsWith("session-")
const EXISTING_DIR = join(home, "proj")
const DEAD_DIR = "/nonexistent/xyzwq"

// ——— песочница: state/queue/db/service.json — всё в HOME-песочнице ———
const stateDir = join(home, ".local/state/uwuwu-dreamer")
const queueDir = join(stateDir, "queue")
const dbPath = join(home, ".local/share/opencode/opencode.db")
mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
mkdirSync(queueDir, { recursive: true })
mkdirSync(join(home, ".local/share/opencode"), { recursive: true })
if (sessionScenario) mkdirSync(EXISTING_DIR, { recursive: true })

// ——— фейк opencode REST: ловим спавн/промпт, отдаём живую дример-сессию ———
// restCall демона ходит на `${base}/api${path}` — мок сматчит ТОЛЬКО точный
// pathname живого контракта (/api/…); двойной префикс (/api/api/…) на живом
// сервере даёт 404 — мок не имеет права его принимать (класс PR #207).
// Незнакомый путь = 404: restCall демона бросает громко, не тихий skip.
// Различимые id спавнов (счётчик): пин «старому legacy-child нет промпта»
// доказуем только если новый спавн не делит id со старым child.
const calls: Array<{ method: string; path: string; body: unknown }> = []
let spawned = 0
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch: async (req) => {
    const url = new URL(req.url)
    const body = req.method === "POST" ? await req.json().catch(() => null) : null
    const path = url.pathname
    calls.push({ method: req.method, path, body })
    if (req.method === "POST" && path === "/api/session") {
      spawned++
      return Response.json({ id: `ses_dreamfake${String(spawned).padStart(2, "0")}` })
    }
    if (req.method === "POST" && /^\/api\/session\/[^/]+\/prompt$/.test(path)) {
      return Response.json({})
    }
    if (req.method === "GET" && path === "/api/session/active") {
      return Response.json({})
    }
    if ((req.method === "GET" || req.method === "DELETE") && /^\/api\/session\/[^/]+$/.test(path)) {
      return req.method === "DELETE" ? Response.json({}) : Response.json({ id: "ses_dreamfake01" })
    }
    return new Response(JSON.stringify({ error: `mock: неожиданный REST ${req.method} ${path}` }), { status: 404 })
  },
})
writeFileSync(join(home, ".local/state/opencode/service.json"), JSON.stringify({
  url: `http://127.0.0.1:${server.port}`, password: "pw",
}))

// ——— фикстурная БД: минимальная схема session_v2 (как в task-run.test.ts) ———
const now = Date.now()
const db = new Database(dbPath)
db.exec(`CREATE TABLE session_v2 (
  id TEXT PRIMARY KEY, title TEXT, directory TEXT, agent TEXT, parent_id TEXT,
  time_created INTEGER, time_updated INTEGER)`)
db.exec(`CREATE TABLE session_message (
  id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER,
  time_created INTEGER, time_updated INTEGER, data TEXT)`)
const ins = db.query(`INSERT INTO session_v2 (id, title, directory, agent, parent_id, time_created, time_updated)
  VALUES (?, ?, ?, ?, ?, ?, ?)`)
if (scenario === "task-spawn") {
  ins.run("ses_dev1", "задача: дев", DIRECTORY, "developer", null, 1000, 5000)
  ins.run("ses_plan1", "план", DIRECTORY, "plan", "ses_dev1", 2000, 4000)
  ins.run("ses_rev1", "задача: ревью", DIRECTORY, "code-quality-reviewer", null, 3000, 6000)
  ins.run("ses_test1", "задача: тест", DIRECTORY, "tester", null, 3500, 7000)
}
if (sessionScenario) {
  const dir = scenario === "session-dead-dir" ? DEAD_DIR : EXISTING_DIR
  // idle-окно тика = 60с (UWUWU_DREAMER_IDLE_MIN=1): time_updated «120с назад» —
  // кандидат; legacy-child молчит дольше — реконсиляция его не трогает
  ins.run("ses_orig1", "крон: ночной аудит", dir, "developer", null, now - 240_000, now - 120_000)
  if (scenario === "session-legacy-migrate") {
    ins.run("ses_dreamlegacy", "💭 крон: ночной аудит", dir, "dreamer", "ses_orig1", now - 200_000, now - 180_000)
  }
}
db.close()

// ——— pre-written state (посессионные сценарии): bootstrapped обязателен —
// иначе первый тик прогонит markBacklog и укатает фикстуру в skipped ———
if (sessionScenario) {
  const sessionsState = scenario === "session-legacy-migrate"
    ? {
        ses_orig1: {
          status: "done", lastProcessedUpdate: now - 180_000, attempts: 1,
          dreamerSes: "ses_dreamlegacy", wokenAt: 0, at: now - 180_000,
        },
      }
    : {}
  writeFileSync(join(stateDir, "state.json"), JSON.stringify({ bootstrapped: true, sessions: sessionsState }))
}

// ——— очередь сигналов: формат фабрики (writeQueueSignal); посессионные
//     сценарии идут с пустой очередью — только один контур за тик ———
if (!sessionScenario) {
  const rootSessions = scenario === "task-spawn"
    ? ["ses_dev1", "ses_rev1", "ses_test1"]
    : ["ses_ghost1"]
  writeFileSync(join(queueDir, `start-${rowId}.json`), JSON.stringify({
    rowId, sessionID: rootSessions[0], ts: 10_000,
  }))
  writeFileSync(join(queueDir, `done-${rowId}.json`), JSON.stringify({
    rowId, project: "uwuwu", title: "тик-тест: задачный прогон",
    rootSessions, branch: "work/task-tick", pr: "",
    envDir: "/home/x/agents-env/ticktest", directory: DIRECTORY, closedAt: 10_000,
  }))
}

// ——— ONCE-тик демона подпроцессом ———
const proc = Bun.spawn(["bun", join(import.meta.dir, "../dreamer/dreamer.ts")], {
  cwd: join(import.meta.dir, ".."),
  env: {
    ...process.env,
    HOME: home,
    UWUWU_DREAMER_STATE_DIR: stateDir,
    UWUWU_DREAMER_QUEUE_DIR: queueDir,
    UWUWU_DREAMER_DB: dbPath,
    UWUWU_DREAMER_ONCE: "1",
    // посессионные сценарии: idle-окно 60с — фикстурная сессия становится
    // кандидатом; задачный контур не зависит от idle-окон и backlog-катоффа
    UWUWU_DREAMER_IDLE_MIN: sessionScenario ? "1" : "999999",
    // пик-окно выключено: спавны не гейтятся временем суток
    UWUWU_DREAMER_PEAK: "",
  },
  stdout: "pipe", stderr: "pipe",
})
const [daemonOut, daemonErr] = await Promise.all([
  new Response(proc.stdout).text(), new Response(proc.stderr).text(),
])
const code = await proc.exited

// Регресс-пин класса PR #207 (строгий pathname-матч): двойной /api-префикс —
// битый прод-путь (живой сервер даёт 404), мок не имеет права его принимать;
// пин ловит полный откат класса и откат протыкаемой ветки; точечный откат
// прочих веток добивают ассерты вердиктов мастер-тестов
const strictPathPin = await fetch(`http://127.0.0.1:${server.port}/api/api/session`, { method: "POST", body: "{}" })
  .then((res) => !res.ok, () => true)
server.stop(true)

// ——— вердикт: state + очередь + перехваченные REST-вызовы ———
const stateFile = join(stateDir, "state.json")
const stateJson = existsSync(stateFile) ? readFileSync(stateFile, "utf-8") : "{}"
const daemonLogFile = join(stateDir, "uwuwu-dreamer.log")
const daemonLog = existsSync(daemonLogFile) ? readFileSync(daemonLogFile, "utf-8") : ""
const state = JSON.parse(stateJson) as {
  sessions?: Record<string, Record<string, unknown>>
  taskRuns?: Record<string, Record<string, unknown>>
  activeSessions?: Record<string, string>
}
const queue = existsSync(queueDir)
  ? (await Array.fromAsync(new Bun.Glob("*.json").scan({ cwd: queueDir })))
  : []
console.log(JSON.stringify({
  exitCode: code,
  daemonLog: daemonLog.trim().slice(-2000),
  daemonStderr: (daemonErr || daemonOut).trim().slice(-500),
  sessions: state.sessions ?? null,
  taskRun: state.taskRuns?.[rowId] ?? null,
  activeSessions: state.activeSessions ?? null,
  queue,
  sessionCreates: calls.filter((c) => c.method === "POST" && c.path === "/api/session"),
  prompts: calls.filter((c) => c.method === "POST" && /^\/api\/session\/[^/]+\/prompt$/.test(c.path)),
  sessionDeletes: calls.filter((c) => c.method === "DELETE" && /^\/api\/session\/[^/]+$/.test(c.path)),
  strictPathPin,
}))
