// dreamer-lib.test.ts — state-машина, кандидаты из sqlite-фикстуры, промпт.
import { afterAll, describe, expect, test } from "bun:test"
import { Database } from "bun:sqlite"
import {
  applyRunOutcome, dreamerPrompt, emptyState, fetchRootSessions, fetchSessionById,
  hasAssistantReply, hasAssistantReplySince, idleBefore, isInPeakWindow, isValidTracked,
  markBacklog, normalizeTracked, parsePeakWindow, pickCandidates, wakeupPrompt,
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
ins({ id: "ses_reviewer", agent: "code-quality-reviewer", updated: NOW - IDLE - 1 }) // ревьювер фабрики — кандидат наравне с девелоперскими
ins({ id: "ses_e2e", title: "[e2e] 📥 Триаж uwuwu", updated: NOW - IDLE - 1 }) // синтетика e2e-рига — вне пост-обработки

afterAll(() => db.close())

const sess = (id: string): SessionRow => {
  const s = fetchSessionById(db, id)
  if (s === null) throw new Error(`фикстура сломана: ${id}`)
  return s
}

describe("fetchRootSessions", () => {
  const rows = fetchRootSessions(db, before)
  test("idle-фильтр и exclusions", () => {
    expect(rows.map((r) => r.id).sort()).toEqual(["ses_noagent", "ses_reviewer", "ses_root_idle"])
  })
  test("NULL-агент не выпадает (COALESCE)", () => {
    expect(rows.some((r) => r.id === "ses_noagent")).toBe(true)
  })
  test("ревьювер фабрики — кандидат наравне с девелоперскими", () => {
    expect(rows.some((r) => r.id === "ses_reviewer")).toBe(true)
  })
  test("e2e-сессия (префикс [e2e]) — вне пост-обработки", () => {
    expect(rows.some((r) => r.id === "ses_e2e")).toBe(false)
  })
  test("fetchSessionById", () => {
    expect(fetchSessionById(db, "ses_root_idle")?.title).toBe("работа")
    expect(fetchSessionById(db, "ses_missing")).toBeNull()
  })
})

describe("pickCandidates", () => {
  const tracked = (over: Partial<TrackedSession>): TrackedSession => ({
    status: "done", lastProcessedUpdate: 999, attempts: 0, dreamerSes: null, wokenAt: 0, at: 0, ...over,
  })
  test("новая сессия", () => {
    const st = emptyState()
    const c = pickCandidates([sess("ses_root_idle")], st, 3)
    expect(c).toEqual([{ session: sess("ses_root_idle"), reason: "new", action: "spawn", resetAttempts: false }])
  })
  test("done и не ожила — не кандидат", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("done и ожила — revived со сбросом попыток", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ lastProcessedUpdate: 1 })
    const c = pickCandidates([sess("ses_root_idle")], st, 3)
    expect(c[0]?.reason).toBe("revived")
    expect(c[0]?.resetAttempts).toBe(true)
  })
  test("gave_up и ожила — тоже revived (не навсегда)", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "gave_up", attempts: 3, lastProcessedUpdate: 1 })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("revived")
  })
  test("failed attempts<max — retry", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "failed", attempts: 2, lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("retry")
  })
  test("failed attempts=max — тишина", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "failed", attempts: 3, lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("running — не кандидат (реконсилиация отдельно)", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "running", dreamerSes: "ses_d1", lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
  test("pending без dreamerSes — stuck-pending (краш до спавна)", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "pending", dreamerSes: null })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.reason).toBe("stuck-pending")
  })
  test("skipped и не ожила — не кандидат", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "skipped", lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)).toEqual([])
  })
})

describe("markBacklog", () => {
  test("старше cutoff — skipped (полный проход, без выборки)", () => {
    const st = emptyState()
    // ses_root_idle: timeUpdated = NOW - IDLE - 1
    const marked = markBacklog(db, st, NOW - 1) // cutoff позже её updated
    expect(marked).toBeGreaterThanOrEqual(1)
    expect(st.sessions.ses_root_idle?.status).toBe("skipped")
    // старые ревью-сессии — та же generic-семантика skipped
    expect(st.sessions.ses_reviewer?.status).toBe("skipped")
    const st2 = emptyState()
    expect(markBacklog(db, st2, 0)).toBe(0) // cutoff раньше всех — не трогаем
    expect(st2.sessions.ses_root_idle).toBeUndefined()
  })
  test("e2e-сессия не размечается и в state не попадает", () => {
    const st = emptyState()
    markBacklog(db, st, NOW - 1)
    expect(st.sessions.ses_e2e).toBeUndefined()
  })
})

describe("applyRunOutcome", () => {
  const base = (): TrackedSession => ({ status: "running", lastProcessedUpdate: 500, attempts: 1, dreamerSes: "ses_d1", wokenAt: 0, at: 0 })
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
    const p = dreamerPrompt({ sessionId: "ses_abc", sessionTitle: "тема", auditPath: "/a/opencode_audit.py", dbPath: "/b/opencode.db" })
    expect(p).toContain("ses_abc")
    expect(p).toContain("opencode_audit.py")
    expect(p).toContain("docs create/append/replace/delete/tree")
    expect(p).toContain("docs read")
    expect(p).toContain("opencode.db")
    expect(p).toContain("uwuwu-cli read")
    expect(p).toContain("одной-двумя строками")
  })
  test("дамп-команда: глобальные флаги ДО сабкоманды (argparse их после — не принимает)", () => {
    // Регресс 20261104072029: форма `session <id> --compact --db <путь>` падала
    // с `unrecognized arguments` — опции объявлены до subparsers.
    for (const p of [
      dreamerPrompt({ sessionId: "ses_abc", sessionTitle: null, auditPath: "/a.py", dbPath: "/b.db" }),
      wakeupPrompt({ sessionId: "ses_abc", sessionTitle: null, auditPath: "/a.py", dbPath: "/b.db", sinceIso: "2026-11-01T12:00:00.000Z" }),
    ]) {
      const cmd = p.split("\n").find((l) => l.includes("python3 \"/a.py\""))
      expect(cmd).toBeDefined()
      expect(cmd).toContain(`--db "/b.db" --compact session ses_abc`)
      expect(cmd).not.toContain(`session ses_abc --`)
    }
  })
  test("валидация state-записей", () => {
    expect(isValidTracked({ status: "done", lastProcessedUpdate: 1, attempts: 0, dreamerSes: null, at: 1 })).toBe(true)
    expect(isValidTracked({ status: "bogus" })).toBe(false)
    expect(isValidTracked(null)).toBe(false)
  })
})

describe("wakeup-модель (1 сессия = 1 дример)", () => {
  const tracked = (over: Partial<TrackedSession>): TrackedSession => ({
    status: "done", lastProcessedUpdate: 1, attempts: 0, dreamerSes: null, wokenAt: 0, at: 0, ...over,
  })

  test("revived с dreamerSes → wakeup, без → spawn", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ dreamerSes: "ses_d1", lastProcessedUpdate: 1 })
    const st2 = emptyState()
    st2.sessions.ses_root_idle = tracked({ dreamerSes: null, lastProcessedUpdate: 1 })
    expect(pickCandidates([sess("ses_root_idle")], st, 3)[0]?.action).toBe("wakeup")
    expect(pickCandidates([sess("ses_root_idle")], st2, 3)[0]?.action).toBe("spawn")
  })

  test("retry с dreamerSes → wakeup той же сессии", () => {
    const st = emptyState()
    st.sessions.ses_root_idle = tracked({ status: "failed", attempts: 1, dreamerSes: "ses_d1", lastProcessedUpdate: sess("ses_root_idle").timeUpdated })
    const c = pickCandidates([sess("ses_root_idle")], st, 3)
    expect(c[0]?.reason).toBe("retry")
    expect(c[0]?.action).toBe("wakeup")
  })

  test("normalizeTracked: старый state без wokenAt → 0", () => {
    const n = normalizeTracked({ status: "done", lastProcessedUpdate: 5, attempts: 0, dreamerSes: null, at: 1 })
    expect(n?.wokenAt).toBe(0)
    expect(normalizeTracked({ status: "bogus" })).toBeNull()
  })
})

describe("hasAssistantReplySince / wakeupPrompt", () => {
  test("ответ после границы считается, до — нет", () => {
    insMsg("ses_d_w", "assistant")
    db.query("UPDATE session_message SET time_created = 1000 WHERE session_id = 'ses_d_w'").run()
    expect(hasAssistantReply(db, "ses_d_w")).toBe(true)
    expect(hasAssistantReplySince(db, "ses_d_w", 1500)).toBe(false)
    db.query("INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES ('mx2', 'ses_d_w', 'assistant', 99, 2000, 0, '{}')").run()
    expect(hasAssistantReplySince(db, "ses_d_w", 1500)).toBe(true)
    expect(hasAssistantReplySince(db, "ses_d_none", 0)).toBe(false)
  })
  test("wakeupPrompt содержит границу и инструкцию дельты", () => {
    const p = wakeupPrompt({ sessionId: "ses_abc", sessionTitle: "т", auditPath: "/a.py", dbPath: "/b.db", sinceIso: "2026-11-01T12:00:00.000Z" })
    expect(p).toContain("2026-11-01T12:00:00.000Z")
    expect(p).toContain("дельту")
    expect(p).toContain("не дублируй")
  })
})

describe("пиковое окно провайдера", () => {
  test("parse: дефолт 15-19, явное значение, мусор → null", () => {
    expect(parsePeakWindow(undefined)).toEqual({ startHour: 15, endHour: 19 })
    expect(parsePeakWindow("9-13")).toEqual({ startHour: 9, endHour: 13 })
    expect(parsePeakWindow("")).toBeNull()
    expect(parsePeakWindow("25-30")).toBeNull()
    expect(parsePeakWindow("abc")).toBeNull()
  })
  test("границы: 14:59 мимо, 15:00 внутри, 18:59 внутри, 19:00 мимо", () => {
    const win = { startHour: 15, endHour: 19 }
    const at = (h: number, m: number) => new Date(2026, 10, 1, h, m)
    expect(isInPeakWindow(at(14, 59), win)).toBe(false)
    expect(isInPeakWindow(at(15, 0), win)).toBe(true)
    expect(isInPeakWindow(at(18, 59), win)).toBe(true)
    expect(isInPeakWindow(at(19, 0), win)).toBe(false)
  })
  test("окно через полночь (22-4)", () => {
    const win = { startHour: 22, endHour: 4 }
    const at = (h: number) => new Date(2026, 10, 1, h)
    expect(isInPeakWindow(at(23), win)).toBe(true)
    expect(isInPeakWindow(at(3), win)).toBe(true)
    expect(isInPeakWindow(at(12), win)).toBe(false)
  })
})
