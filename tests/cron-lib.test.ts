// tests/cron-lib.test.ts — тесты чистых функций cron-mode (bun test)
import { test, expect, describe } from "bun:test"
import {
  parseDuration,
  parseHHMM,
  parseSchedule,
  parseCronFields,
  nextCronFire,
  nextFire,
  renderBrief,
  describeSchedule,
  validateTaskInput,
  peakHoursWarning,
  compactResult,
  newId,
  type CronTask,
} from "../lib/cron-lib"

const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

// локальные даты (система в Asia/Tokyo — как прод)
const at = (day: number, h: number, m = 0): number => new Date(2026, 9, day, h, m, 0, 0).getTime()
// 9 октября 2026 = пятница; 13 = вторник; 16 = пятница

describe("parseDuration", () => {
  test("единицы", () => {
    expect(parseDuration("90s")).toBe(90_000)
    expect(parseDuration("20m")).toBe(20 * MIN)
    expect(parseDuration("2h")).toBe(2 * HOUR)
    expect(parseDuration("1d")).toBe(DAY)
  })
  test("мусор → null", () => {
    expect(parseDuration("20")).toBeNull()
    expect(parseDuration("m")).toBeNull()
    expect(parseDuration("-5m")).toBeNull()
    expect(parseDuration("")).toBeNull()
  })
})

describe("parseHHMM", () => {
  test("ок", () => {
    expect(parseHHMM("15:00")).toEqual({ h: 15, m: 0 })
    expect(parseHHMM("9:05")).toEqual({ h: 9, m: 5 })
  })
  test("мусор", () => {
    expect(parseHHMM("25:00")).toBeNull()
    expect(parseHHMM("15:60")).toBeNull()
    expect(parseHHMM("15")).toBeNull()
  })
})

describe("parseSchedule", () => {
  const now = at(9, 12)
  test("in → once", () => {
    expect(parseSchedule({ in: "20m" }, now)).toEqual({ kind: "once", at: now + 20 * MIN })
  })
  test("at в будущем → сегодня", () => {
    expect(parseSchedule({ at: "15:00" }, now)).toEqual({ kind: "once", at: at(9, 15) })
  })
  test("at в прошлом → завтра", () => {
    expect(parseSchedule({ at: "09:00" }, now)).toEqual({ kind: "once", at: at(10, 9) })
  })
  test("every короче минуты → null", () => {
    expect(parseSchedule({ every: "30s" }, now)).toBeNull()
    expect(parseSchedule({ every: "5m" }, now)).toEqual({ kind: "every", intervalMs: 5 * MIN })
  })
  test("конфликт двух полей → null", () => {
    expect(parseSchedule({ in: "20m", every: "5m" }, now)).toBeNull()
  })
  test("лишние не-временные поля (name/prompt) не мешают — баг E2E", () => {
    expect(parseSchedule({ every: "2m" } as never, now)).toEqual({ kind: "every", intervalMs: 2 * MIN })
    const withExtras = { name: "x", prompt: "y", reportPolicy: "z", ...{ every: "2m" } }
    expect(parseSchedule(withExtras as never, now)).toEqual({ kind: "every", intervalMs: 2 * MIN })
  })
  test("пусто → null", () => {
    expect(parseSchedule({}, now)).toBeNull()
  })
  test("cron валидный/невалидный", () => {
    expect(parseSchedule({ cron: "0 9 * * 1-5" }, now)).toEqual({ kind: "cron", expr: "0 9 * * 1-5" })
    expect(parseSchedule({ cron: "0 9 * *" }, now)).toBeNull()
  })
})

describe("parseCronFields", () => {
  test("7 = воскресенье", () => {
    const f = parseCronFields("0 0 * * 7")
    expect(f?.dow.values.has(0)).toBe(true)
  })
  test("шаги и диапазоны", () => {
    const f = parseCronFields("*/15 9-17 * * 1,3,5")
    expect(f?.min.values.has(15)).toBe(true)
    expect(f?.min.values.has(16)).toBe(false)
    expect(f?.hour.values.has(17)).toBe(true)
    expect(f?.dow.values.has(3)).toBe(true)
  })
  test("мусор → null", () => {
    expect(parseCronFields("60 * * * *")).toBeNull()
    expect(parseCronFields("a b c d e")).toBeNull()
    expect(parseCronFields("* * * */2 *")).not.toBeNull()
  })
})

describe("nextCronFire", () => {
  test("будни 9:00: пт вечер → пн утро", () => {
    expect(nextCronFire("0 9 * * 1-5", at(9, 18, 30))).toBe(at(12, 9))
  })
  test("*/15: 10:07 → 10:15", () => {
    expect(nextCronFire("*/15 * * * *", at(9, 10, 7))).toBe(at(9, 10, 15))
  })
  test("ровно сейчас → следующая минута подхода", () => {
    expect(nextCronFire("0 9 * * *", at(9, 9))).toBe(at(10, 9))
  })
  test("vixie OR: dom или dow (13-е ИЛИ пятница)", () => {
    // from пт 09.10 13:00 → 13.10 вт 12:00 (dom) раньше, чем пт 16.10 (dow)
    expect(nextCronFire("0 12 13 * 5", at(9, 13))).toBe(at(13, 12))
  })
})

describe("nextFire", () => {
  test("once — точка", () => {
    expect(nextFire({ kind: "once", at: 1234 }, 999)).toBe(1234)
  })
  test("every — from + интервал", () => {
    expect(nextFire({ kind: "every", intervalMs: 5 * MIN }, at(9, 10))).toBe(at(9, 10) + 5 * MIN)
  })
})

describe("validateTaskInput", () => {
  test("ок", () => {
    expect(validateTaskInput({ prompt: "проверь квоты" })).toEqual({ ok: true, prompt: "проверь квоты" })
  })
  test("пустой/короткий", () => {
    expect(validateTaskInput({}).ok).toBe(false)
    expect(validateTaskInput({ prompt: "аб" }).ok).toBe(false)
  })
})

describe("renderBrief", () => {
  const task: CronTask = {
    id: "a3f2",
    name: "Проверка квот",
    sessionID: "ses_x",
    prompt: "Проверь квоты через API.",
    schedule: { kind: "every", intervalMs: 2 * HOUR },
    createdAt: at(9, 14),
    createdBy: "агент",
    lastFiredAt: at(9, 16),
    lastResult: "61% weekly",
    fireCount: 3,
  }
  test("бриф самодостаточен", () => {
    const b = renderBrief(task, at(9, 18))
    expect(b).toContain("[cron #a3f2]")
    expect(b).toContain("Проверка квот")
    expect(b).toContain("каждые 2 ч")
    expect(b).toContain("Проверь квоты через API.")
    expect(b).toContain("61% weekly")
    expect(b).toContain("cron_remove")
  })
  test("без истории — без блока прошлого прогона", () => {
    const b = renderBrief({ ...task, lastResult: undefined, lastFiredAt: undefined }, at(9, 18))
    expect(b).not.toContain("Прошлый прогон")
  })
})

describe("peakHoursWarning (будни 15:00–19:00 JST)", () => {
  test("пн 16:00 — предупреждение", () => {
    expect(peakHoursWarning(at(12, 16))).not.toBe("")
  })
  test("пн 19:00 — нет", () => {
    expect(peakHoursWarning(at(12, 19))).toBe("")
  })
  test("сб 16:00 — нет", () => {
    expect(peakHoursWarning(at(10, 16))).toBe("") // 10.10.2026 = суббота
  })
})

describe("прочее", () => {
  test("compactResult режет с хвоста-многоточием", () => {
    expect(compactResult("abc")).toBe("abc")
    expect(compactResult("x".repeat(600)).length).toBe(500)
  })
  test("newId уникален", () => {
    const taken = new Set(["0000", "ffff"])
    const id = newId((i) => taken.has(i))
    expect(taken.has(id)).toBe(false)
    expect(id).toMatch(/^[0-9a-f]{4}$/)
  })
  test("describeSchedule по-русски", () => {
    expect(describeSchedule({ kind: "every", intervalMs: 2 * HOUR })).toContain("каждые 2 ч")
    expect(describeSchedule({ kind: "once", at: at(9, 15) })).toContain("однократно")
  })
})
