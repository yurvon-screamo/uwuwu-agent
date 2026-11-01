// dreamer-lib.test.ts — state-машина, кандидаты из sqlite-фикстуры, промпт.
import { afterAll, describe, expect, test } from "bun:test"
import { Database } from "bun:sqlite"
import {
  applyRunOutcome, dreamerPrompt, emptyState, fetchRootSessions, fetchSessionById,
  hasAssistantReply, idleBefore, isValidTracked, markBacklog, pickCandidates,
  type SessionRow, type TrackedSession,
} from "./dreamer-lib"

// ——— фикстурная БД: минимальная схема session_v2/session_message ———

let seq = 0
const db = new Database(":memory:")
db.exec(`CREATE TABLE session_v2 (
  id TEXT PRIMARY KEY, title TEXT, directory TEXT, agent TEXT, parent_id TEXT,
  time_created INTEGER, time_updated INTEGER)`)
db.exec(`CREATE TABLE session_message (
  id TEXT, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER,
  time_updated INTEGER, data TEXT)`)

function ins(s: {
  id: string; title?: string; agent?: string | null; parent?: string | null
  created?: number; updated?: number
}): void {
  db.query(`INSERT INTO session_v2 (id, title, directory, agent, parent_id, time_created, time_updated)
            VALUES (?, ?, NULL, ?, ?, ?, ?)`)
    .run(s.id, s.title ?? null, s.agent ?? "developer", s.parent ?? null,
      s.created ?? 1000, s.updated ?? 1000)
}

function insMsg(ses: string, type: string): void {
  seq += 1
  db.query(`INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
            VALUES (?, ?, ?, ?, 0, 0, '{}')`).run(`m${seq}`, ses, type, seq)
}

const NOW = 1_000_000
const IDLE = 30_000
const before = idleBefore(NOW, IDLE)

ins({ id: "ses_root_idle", title: "работа", updated: NOW - IDLE - 1 })        // кандидат
ins({ id: "ses_root_fresh", title: "живая", updated: NOW - 1000 })            // моложе idle
ins({ id: "ses_dreamer", agent: "dreamer", updated: NOW - IDLE - 1 })         // сам дример
ins({ id: "ses_child", parent: "ses_root_idle", updated: NOW - IDLE - 1 })    // сабагент
ins({ id: "ses_noagent", agent: null, updated: NOW - IDLE - 1 })              // NULL-агент — кандидат

afterAll(() => db.close())

const sess = (id: string): SessionRow => {
  const s = fetchSessionById(db, id)
  if (s === null) throw new Error(`фикстура сломана: ${id}`)
  return s
}

describe("fetchRootSessions", () => {
  const rows = fetchRootSessions(db, before)
  test("idle-фильтр и exclusions", () => {
    expect(rows.map((r) => r.id).sort()).toEqual(["ses_noagent", "ses_root_idle"])
  })
  test("NULL-агент не выпадает (COALESCE)", () => {
    expect(rows.some((r) => r.id === "ses_noagent")).toBe(true)
  })
  test("fetchSessionById", () => {
    expect(fetchSessionById(db, "ses_root_idle")?.title).toBe("работа")
    expect(fetchSessionById(db, "ses_missing")).toBeNull()
  })
})

describe("pickCandidates", () => {
  const tracked = (over: Partial<TrackedSession>): TrackedSession => ({
    status: "done", lastProcessedUpdate: 999, attempts: 0, dreamerSes: null, at: 0, ...over,
  })
  test("новая сессия", () => {
    const st = emptyState()
    const c = pickCandidates([sess("ses_root_idle")], st, 3)
    expect(c).toEqual([{ session: sess("ses_root_idle"), reason: "new", resetAttempts: false }])
  })
  test("done и не ожила — не кандидат", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("done и ожила — revived со сбросом попыток", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ lastProcessedUpdate: 1 })
    const c = pickCandidates([sess("ses_root_idle")], st, 3)
    expect(c[0]?.reason).toBe("revived")
    expect(c[0]?.resetAttempts).toBe(true)
  })
  test("gave_up и ожила — тоже revived (не навсегда)", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "gave_up", attempts: 3, lastProcessedUpdate: 1 })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("revived")
  })
  test("failed attempts<max — retry", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "failed", attempts: 2, lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("retry")
  })
  test("failed attempts=max — тишина", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "failed", attempts: 3, lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("running — не кандидат (реконсилиация отдельно)", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "running", dreamerSes: "ses_d1", lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("pending без dreamerSes — stuck-pending (краш до спавна)", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "pending", dreamerSes: null })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("stuck-pending")
  })
  test("skipped и не ожила — не кандидат", () => {
    const st = emptyState()
    st.sessions["ses_root_idle"] = tracked({ status: "skipped", lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
})

describe("markBacklog", () => {
  test("старше cutoff — skipped (полный проход, без выборки)", () => {
    const st = emptyState()
    // ses_root_idle: timeUpdated = NOW - IDLE - 1
    const marked = markBacklog(db, st, NOW - 1) // cutoff позже её updated
    expect(marked).toBeGreaterThanOrEqual(1)
    expect(st.sessions["ses_root_idle"]?.status).toBe("skipped")
    const st2 = emptyState()
    expect(markBacklog(db, st2, 0)).toBe(0) // cutoff раньше всех — не трогаем
    expect(st2.sessions["ses_root_idle"]).toBeUndefined()
  })
})

describe("applyRunOutcome", () => {
  const base = (): TrackedSession => ({ status: "running", lastProcessedUpdate: 500, attempts: 1, dreamerSes: "ses_d1", at: 0 })
  const s = sess("ses_root_idle")
  test("живой прогон — keep", () => {
    expect(applyRunOutcome(base(), s, { replied: false, dead: false, timedOut: false }, 3)).toBe("keep")
  })
  test("мёртвый с ответом — done + snapshot timeUpdated", () => {
    const t = base()
    expect(applyRunOutcome(t, s, { replied: true, dead: true, timedOut: false }, 3)).toBe("done")
    expect(t.lastProcessedUpdate).toBe(s.timeUpdated)
  })
  test("мёртвый без ответа — failed, затем gave_up", () => {
    const t = base()
    expect(applyRunOutcome(t, s, { replied: false, dead: true, timedOut: false }, 3)).toBe("failed")
    expect(t.attempts).toBe(2)
    t.attempts = 2
    expect(applyRunOutcome(t, s, { replied: false, dead: true, timedOut: false }, 3)).toBe("gave_up")
  })
  test("таймаут без ответа — failed", () => {
    expect(applyRunOutcome(base(), s, { replied: false, dead: false, timedOut: true }, 3)).toBe("failed")
  })
})

describe("hasAssistantReply", () => {
  test("только assistant считается", () => {
    insMsg("ses_d_x", "user")
    expect(hasAssistantReply(db, "ses_d_x")).toBe(false)
    insMsg("ses_d_x", "assistant")
    expect(hasAssistantReply(db, "ses_d_x")).toBe(true)
    expect(hasAssistantReply(db, "ses_d_none")).toBe(false)
  })
})

describe("dreamerPrompt / isValidTracked", () => {
  test("промпт содержит входы", () => {
    const p = dreamerPrompt({ sessionId: "ses_abc", sessionTitle: "тема", auditPath: "/a/opencode_audit.py", wikiOpsPath: "/a/wiki_ops.py", dbPath: "/b/opencode.db" })
    expect(p).toContain("ses_abc")
    expect(p).toContain("opencode_audit.py")
    expect(p).toContain("wiki_ops.py")
    expect(p).toContain("opencode.db")
    expect(p).toContain("bash")
  })
  test("валидация state-записей", () => {
    expect(isValidTracked({ status: "done", lastProcessedUpdate: 1, attempts: 0, dreamerSes: null, at: 1 })).toBe(true)
    expect(isValidTracked({ status: "bogus" })).toBe(false)
    expect(isValidTracked(null)).toBe(false)
  })
})
