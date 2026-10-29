// cron-lib.test.ts — расписания, границы, промпты.
import { describe, expect, test } from "bun:test"
import {
  parseSchedule, fieldMatches, due, isOneShot, isValidTracked,
  firePrompt, cronBodyStub, minuteStart,
} from "./cron-lib"

describe("parseSchedule", () => {
  test("интервалы", () => {
    expect(parseSchedule("5m")).toEqual({ kind: "interval", ms: 300_000 })
    expect(parseSchedule("2h")).toEqual({ kind: "interval", ms: 7_200_000 })
    expect(parseSchedule("1d")).toEqual({ kind: "interval", ms: 86_400_000 })
    expect(parseSchedule("0m")).toBeNull()
    expect(parseSchedule("5s")).toBeNull()
  })
  test("at", () => {
    expect(parseSchedule("09:30")).toEqual({ kind: "at", hour: 9, minute: 30 })
    expect(parseSchedule("23:59 01.12")).toEqual({ kind: "at", hour: 23, minute: 59, day: 1, month: 12 })
    expect(parseSchedule("25:00")).toBeNull()
  })
  test("cron-выражения", () => {
    expect(parseSchedule("*/30 * * * *")?.kind).toBe("cron")
    expect(parseSchedule("* * * *")).toBeNull()
    expect(parseSchedule("раз в минутку")).toBeNull()
  })
})

describe("fieldMatches", () => {
  test("звезда", () => expect(fieldMatches("*", 17, 0, 59)).toBe(true))
  test("число и список", () => {
    expect(fieldMatches("5", 5, 0, 59)).toBe(true)
    expect(fieldMatches("5", 6, 0, 59)).toBe(false)
    expect(fieldMatches("1,15,30", 15, 0, 59)).toBe(true)
    expect(fieldMatches("1,15,30", 16, 0, 59)).toBe(false)
  })
  test("диапазон и шаг", () => {
    expect(fieldMatches("9-17", 12, 0, 23)).toBe(true)
    expect(fieldMatches("9-17", 18, 0, 23)).toBe(false)
    expect(fieldMatches("*/15", 45, 0, 59)).toBe(true)
    expect(fieldMatches("*/15", 47, 0, 59)).toBe(false)
    expect(fieldMatches("1-59/15", 46, 0, 59)).toBe(true) // 1,16,31,46
  })
})

describe("due", () => {
  test("interval: первый раз — сразу", () => {
    const s = parseSchedule("5m")!
    expect(due(s, 0, new Date())).toBe(true)
  })
  test("interval: не раньше N", () => {
    const s = parseSchedule("5m")!
    const now = new Date("2026-10-25T12:00:00")
    expect(due(s, now.getTime() - 4 * 60_000, now)).toBe(false)
    expect(due(s, now.getTime() - 5 * 60_000, now)).toBe(true)
  })
  test("cron: минута совпала и не стреляли в эту минуту", () => {
    const s = parseSchedule("*/30 * * * *")!
    const now = new Date("2026-10-25T12:30:00")
    expect(due(s, minuteStart(now) - 60_000, now)).toBe(true)
    expect(due(s, minuteStart(now), now)).toBe(false) // уже стреляли
    const off = new Date("2026-10-25T12:31:00")
    expect(due(s, 0, off)).toBe(false)
  })
  test("at: своя минута и догон, один раз", () => {
    const s = parseSchedule("09:30")!
    expect(due(s, 0, new Date("2026-10-25T09:30:00"))).toBe(true)
    expect(due(s, minuteStart(new Date("2026-10-25T09:30:00")), new Date("2026-10-25T09:30:30"))).toBe(false)
    expect(due(s, 0, new Date("2026-10-25T09:31:00"))).toBe(true) // догон после дауна
    expect(due(s, 0, new Date("2026-10-25T09:29:00"))).toBe(false) // рано
  })
  test("at с датой", () => {
    const s = parseSchedule("09:30 01.12")!
    expect(due(s, 0, new Date("2026-12-01T09:30:00"))).toBe(true)
    expect(due(s, 0, new Date("2026-11-30T09:30:00"))).toBe(false)
  })
  test("isOneShot", () => {
    expect(isOneShot(parseSchedule("09:30")!)).toBe(true)
    expect(isOneShot(parseSchedule("5m")!)).toBe(false)
  })
})

describe("isValidTracked", () => {
  test("формы", () => {
    expect(isValidTracked({ sessionID: "", lastFiredAt: 0 })).toBe(true)
    expect(isValidTracked({ sessionID: "ses_1", lastFiredAt: 5, firedAtIso: "x" })).toBe(true)
    expect(isValidTracked({ sessionID: "gh_1", lastFiredAt: 5 })).toBe(false)
    expect(isValidTracked({ sessionID: "", lastFiredAt: "5" })).toBe(false)
  })
})

describe("firePrompt", () => {
  const row = { rowId: "r1", bodyDoc: "d1", title: "Проверка X", schedule: "*/30 * * * *", status: "active", lastRun: "" }
  test("контракт: коммент с 🤖, [DONE] для разовых, границы", () => {
    const p = firePrompt(row, "=== СЕКРЕТ ===\nпроверь и доложи")
    expect(p).toContain("uwuwu-cli task comment r1")
    expect(p).not.toContain("🤖")
    expect(p).toContain("[DONE]")
    expect(p).toContain("≡")
  })
  test("пустое тело — стаб по заголовку", () => {
    expect(firePrompt(row, "")).toContain("(пусто — отработай по заголовку)")
  })
})

describe("cronBodyStub", () => {
  test("структура тела", () => {
    const md = cronBodyStub("Проверка Y", "5m")
    expect(md).toContain("# Проверка Y")
    expect(md).toContain("`5m`")
    expect(md).toContain("## Лог")
  })
})

describe("parseSchedule: мусорные cron-поля", () => {
  test("abc в поле → null (не молчащий навсегда крон)", () => {
    expect(parseSchedule("abc * * * *")).toBeNull()
    expect(parseSchedule("* 25 * * *")).toBeNull() // 25 часа — вне диапазона, теперь null
    expect(parseSchedule("*/ * * * *")).toBeNull()
    expect(parseSchedule("1-2-3 * * * *")).toBeNull()
  })
})

describe("at: догон пропущенной минуты", () => {
  test("минута прошла пока демон лежал — стреляем поздно", () => {
    const s = parseSchedule("09:30")!
    expect(due(s, 0, new Date("2026-10-25T09:37:12"))).toBe(true)
  })
  test("минута ещё не наступила — нет", () => {
    const s = parseSchedule("09:30")!
    expect(due(s, 0, new Date("2026-10-25T09:29:00"))).toBe(false)
  })
})
