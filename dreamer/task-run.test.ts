// task-run.test.ts — задачный контур: парсинг очереди, сбор сессий задачи,
// state-машина прогона, скип активных задач в посессионной выборке, промпт.
import { afterAll, describe, expect, test } from "bun:test"
import { Database } from "bun:sqlite"
import {
  acceptDoneSignal, applyTaskRunOutcome, batchMaxUpdated, collectTaskSessions,
  dropTaskActiveSessions, mergeActiveSession, normalizeActiveSessions,
  normalizeTrackedRun, parseQueuedSignal, pickTaskRunCandidates, taskRunPrompt,
  type TaskDoneSignal, type TrackedTaskRun,
} from "./task-run-lib"
import { fetchRootSessions } from "./dreamer-lib"

// ——— фикстурная БД: минимальная схема session_v2 ———

const db = new Database(":memory:")
db.exec(`CREATE TABLE session_v2 (
  id TEXT PRIMARY KEY, title TEXT, directory TEXT, agent TEXT, parent_id TEXT,
  time_created INTEGER, time_updated INTEGER)`)

function ins(s: {
  id: string; title?: string; agent?: string | null; parent?: string | null
  created?: number; updated?: number
}): void {
  db.query(`INSERT INTO session_v2 (id, title, directory, agent, parent_id, time_created, time_updated)
            VALUES (?, ?, NULL, ?, ?, ?, ?)`)
    .run(s.id, s.title ?? null, s.agent ?? "developer", s.parent ?? null,
      s.created ?? 1000, s.updated ?? 1000)
}

afterAll(() => db.close())

// ——— парсинг очереди ———

const startSignal = {
  rowId: "20261108000000-abc123",
  sessionID: "ses_dev1",
  ts: 1_000,
}

const doneSignal: TaskDoneSignal = {
  rowId: "20261108000000-abc123",
  project: "uwuwu",
  title: "задача",
  rootSessions: ["ses_dev1", "ses_rev1"],
  branch: "work/task-r",
  pr: "https://github.com/x/y/pull/1",
  envDir: "/home/x/agents-env/r",
  directory: "/home/x/uwuwu-agent",
  closedAt: 2_000,
}

describe("parseQueuedSignal", () => {
  test("start-файл парсится в start-сигнал", () => {
    const name = `start-${startSignal.rowId}.json`
    expect(parseQueuedSignal(name, JSON.stringify(startSignal))).toEqual({ kind: "start", signal: startSignal })
  })

  test("done-файл парсится в done-сигнал", () => {
    const name = `done-${doneSignal.rowId}.json`
    expect(parseQueuedSignal(name, JSON.stringify(doneSignal))).toEqual({ kind: "done", signal: doneSignal })
  })

  test("битый JSON — null (демон удалит файл с логом)", () => {
    expect(parseQueuedSignal(`start-${startSignal.rowId}.json`, "{broken")).toBeNull()
  })

  test("невалидные поля — null (sessionID не ses_, rootSessions не ses_-список)", () => {
    const name = `start-${startSignal.rowId}.json`
    expect(parseQueuedSignal(name, JSON.stringify({ ...startSignal, sessionID: "garbage" }))).toBeNull()
    const doneName = `done-${doneSignal.rowId}.json`
    expect(parseQueuedSignal(doneName, JSON.stringify({ ...doneSignal, rootSessions: ["x"] }))).toBeNull()
    expect(parseQueuedSignal(doneName, JSON.stringify({ ...doneSignal, rootSessions: "ses_dev1" }))).toBeNull()
  })

  test("чужое имя файла — null", () => {
    expect(parseQueuedSignal("random.json", "{}")).toBeNull()
    expect(parseQueuedSignal("start-хух.json", "{}")).toBeNull()
  })
})

// ——— активный набор ———

describe("active sessions helpers", () => {
  test("мерж и снятие по rowId", () => {
    const active = normalizeActiveSessions(undefined)
    expect(active).toEqual({})
    mergeActiveSession(active, "ses_a", "row-1")
    mergeActiveSession(active, "ses_b", "row-1")
    mergeActiveSession(active, "ses_c", "row-2")
    dropTaskActiveSessions(active, "row-1")
    expect(active).toEqual({ ses_c: "row-2" })
  })

  test("normalize фильтрует мусор state", () => {
    const active = normalizeActiveSessions({ ses_ok: "row-1", junk: "row-2", ses_bad: 42 })
    expect(active).toEqual({ ses_ok: "row-1" })
  })
})

// ——— адмиссия задачных прогонов (ретраи failed) ———

describe("pickTaskRunCandidates", () => {
  const track = (over: Partial<TrackedTaskRun>): TrackedTaskRun => ({
    status: "pending", project: "uwuwu", title: "t", rootSessions: ["ses_a"],
    branch: "b", pr: "", envDir: "", directory: "/x", closedAt: 1,
    lastProcessedUpdate: 0, attempts: 0, dreamerSes: null, wokenAt: 0, at: 0,
    ...over,
  })

  test("pending без дример-сессии — spawn", () => {
    const cands = pickTaskRunCandidates({ row1: track({}) }, 3)
    expect(cands).toHaveLength(1)
    expect(cands[0]).toMatchObject({ rowId: "row1", action: "spawn" })
  })

  test("pending с живой дример-сессией (stuck-pending) — wakeup", () => {
    const cands = pickTaskRunCandidates({ row1: track({ dreamerSes: "ses_dream" }) }, 3)
    expect(cands[0]?.action).toBe("wakeup")
  })

  test("failed с живыми попытками — ретрай (wakeup той же сессии)", () => {
    const cands = pickTaskRunCandidates({ row1: track({ status: "failed", attempts: 1, dreamerSes: "ses_dream" }) }, 3)
    expect(cands).toHaveLength(1)
    expect(cands[0]?.action).toBe("wakeup")
  })

  test("failed с живыми попытками и без дример-сессии — respawn", () => {
    const cands = pickTaskRunCandidates({ row1: track({ status: "failed", attempts: 2 }) }, 3)
    expect(cands).toHaveLength(1)
    expect(cands[0]?.action).toBe("spawn")
  })

  test("failed на исчерпании попыток — не кандидат (gave_up снимет активный набор в реконсиле)", () => {
    expect(pickTaskRunCandidates({ row1: track({ status: "failed", attempts: 3 }) }, 3)).toEqual([])
  })

  test("running и финальные статусы — не кандидаты", () => {
    const cands = pickTaskRunCandidates({
      row_running: track({ status: "running", dreamerSes: "ses_d" }),
      row_done: track({ status: "done" }),
      row_gave_up: track({ status: "gave_up", attempts: 3 }),
    }, 3)
    expect(cands).toEqual([])
  })
})

// ——— сбор сессий задачи ———

describe("collectTaskSessions", () => {
  // структура: ses_dev1 (корень) → ses_child1 (сабагент) → ses_grandchild;
  // ses_rev1 (корень, ревьювер); ses_dead_dev — осиротевший корень прошлого
  // рестарта; ses_dreamer_child — ребёнок с agent=dreamer (исключается);
  // ses_e2e_child — исключается
  ins({ id: "ses_dev1", parent: null, created: 100, updated: 900 })
  ins({ id: "ses_child1", parent: "ses_dev1", created: 200, updated: 800 })
  ins({ id: "ses_grandchild", parent: "ses_child1", created: 300, updated: 700 })
  ins({ id: "ses_rev1", parent: null, agent: "code-quality-reviewer", created: 400, updated: 600 })
  ins({ id: "ses_dead_dev", parent: null, created: 50, updated: 90 })
  ins({ id: "ses_dreamer_child", parent: "ses_dev1", agent: "dreamer", created: 250, updated: 250 })
  ins({ id: "ses_e2e_child", parent: "ses_dev1", title: "[e2e] 📥 смоук", created: 260, updated: 260 })
  ins({ id: "ses_outsider", parent: null, created: 10, updated: 20 })

  test("BFS от полной истории корней накрывает все поколения задачи", () => {
    const ids = collectTaskSessions(db, ["ses_dev1", "ses_rev1", "ses_dead_dev"]).map((s) => s.id)
    expect(ids.sort()).toEqual(["ses_child1", "ses_dead_dev", "ses_dev1", "ses_grandchild", "ses_rev1"])
  })

  test("dreamer- и e2e-дети исключаются", () => {
    const ids = collectTaskSessions(db, ["ses_dev1"]).map((s) => s.id)
    expect(ids).not.toContain("ses_dreamer_child")
    expect(ids).not.toContain("ses_e2e_child")
  })

  test("лимит гвардит взрыв графа", () => {
    const ids = collectTaskSessions(db, ["ses_dev1", "ses_rev1", "ses_dead_dev"], 3)
    expect(ids.length).toBe(3)
  })

  test("несуществующие корни молча пропускаются (чистка БД руками)", () => {
    expect(collectTaskSessions(db, ["ses_missing"])).toEqual([])
  })

  test("batchMaxUpdated — max timeUpdated батча", () => {
    expect(batchMaxUpdated(collectTaskSessions(db, ["ses_dev1"]))).toBe(900)
  })
})

// ——— state-машина задачного прогона ———

const runTrack = (over: Partial<TrackedTaskRun>): TrackedTaskRun => ({
  status: "running", project: "uwuwu", title: "t", rootSessions: ["ses_a"],
  branch: "b", pr: "", envDir: "", directory: "/x", closedAt: 1,
  lastProcessedUpdate: 0, attempts: 0, dreamerSes: "ses_dream", wokenAt: 0, at: 0,
  ...over,
})

describe("applyTaskRunOutcome", () => {
  test("живой дример без таймаута — keep", () => {
    const t = runTrack({})
    expect(applyTaskRunOutcome(t, 100, { replied: false, dead: false, timedOut: false }, 3)).toBe("keep")
    expect(t.status).toBe("running")
  })

  test("ответивший дример — done с границей дедупа", () => {
    const t = runTrack({ lastProcessedUpdate: 50 })
    expect(applyTaskRunOutcome(t, 100, { replied: true, dead: true, timedOut: false }, 3)).toBe("done")
    expect(t.status).toBe("done")
    expect(t.lastProcessedUpdate).toBe(100)
  })

  test("молчащий зависший — failed, на исчерпании — gave_up", () => {
    const t = runTrack({ attempts: 2 })
    expect(applyTaskRunOutcome(t, 100, { replied: false, dead: false, timedOut: true }, 3)).toBe("gave_up")
    expect(t.status).toBe("gave_up")
    const t2 = runTrack({ attempts: 0 })
    expect(applyTaskRunOutcome(t2, 100, { replied: false, dead: true, timedOut: false }, 3)).toBe("failed")
    expect(t2.status).toBe("failed")
  })
})

describe("acceptDoneSignal", () => {
  test("новая задача — pending с нулевой границей дедупа", () => {
    const t = acceptDoneSignal(undefined, doneSignal, 999)
    expect(t.status).toBe("pending")
    expect(t.lastProcessedUpdate).toBe(0)
    expect(t.attempts).toBe(0)
    expect(t.rootSessions).toEqual(["ses_dev1", "ses_rev1"])
    expect(t.closedAt).toBe(2_000)
  })

  test("переоткрытие — граница дедупа прошлого прогона сохраняется", () => {
    const prev = runTrack({ status: "done", lastProcessedUpdate: 5_000 })
    const t = acceptDoneSignal(prev, doneSignal, 999)
    expect(t.status).toBe("pending")
    expect(t.lastProcessedUpdate).toBe(5_000)
    expect(t.attempts).toBe(0)
  })

  test("normalize round-trip", () => {
    const t = acceptDoneSignal(undefined, doneSignal, 999)
    expect(normalizeTrackedRun(normalizeTrackedRun(t))).toEqual(t)
    expect(normalizeTrackedRun({ status: "weird" })).toBeNull()
  })
})

// ——— посессионная выборка: скип активных задач ———

describe("fetchRootSessions active-skip", () => {
  ins({ id: "ses_free_idle", title: "свободная", created: 4_000, updated: 4_000 })
  ins({ id: "ses_task_live", title: "сессия живой задачи", created: 4_500, updated: 4_000 })

  test("сессия живой задачи не попадает в посессионный пул", () => {
    const active = { ses_task_live: "row-live" }
    const rows = fetchRootSessions(db, 5_000, 200, active)
    expect(rows.some((r) => r.id === "ses_free_idle")).toBe(true)
    expect(rows.some((r) => r.id === "ses_task_live")).toBe(false)
  })

  test("без активного набора — поведение прежнее", () => {
    const rows = fetchRootSessions(db, 5_000)
    expect(rows.some((r) => r.id === "ses_task_live")).toBe(true)
  })
})

// ——— промпт ———

describe("taskRunPrompt", () => {
  const input = {
    rowId: "20261108000000-abc123",
    project: "uwuwu",
    title: "тайтл",
    branch: "work/task-r",
    pr: "https://github.com/x/y/pull/1",
    directory: "/home/x/uwuwu-agent",
    lastProcessedUpdate: 0,
    sessions: collectTaskSessions(db, ["ses_dev1"]),
    auditPath: "/audit.py",
    wikiOpsPath: "/wiki_ops.py",
    dbPath: "/opencode.db",
  }

  test("первый прогон: тело, дампы по сессиям, git-контекст", () => {
    const prompt = taskRunPrompt(input)
    expect(prompt).toContain("task get 20261108000000-abc123 --project uwuwu")
    expect(prompt).toContain("--compact session ses_dev1")
    expect(prompt).toContain("--compact session ses_child1")
    expect(prompt).not.toContain("--compact session ses_dreamer_child")
    expect(prompt).toContain("gh pr view https://github.com/x/y/pull/1")
    expect(prompt).toContain("/home/x/uwuwu-agent")
    expect(prompt).not.toContain("Повторный прогон")
  })

  test("переоткрытие: граница дедупа в промпте", () => {
    const prompt = taskRunPrompt({ ...input, lastProcessedUpdate: 1_234_567 })
    expect(prompt).toContain("Повторный прогон")
    expect(prompt).toContain("1234567")
  })

  test("ретрай помечен как повторная попытка", () => {
    const prompt = taskRunPrompt({ ...input, retry: true })
    expect(prompt).toContain("повторная попытка")
  })
})
