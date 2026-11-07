// lead-lib.test.ts — тесты чистых функций ночного шедулера uwuwu-lead
// (bun test, без сети)
import { test, expect } from "bun:test"
import { auditDue, composeAuditPrompt, localDateKey, parseAuditAt, validLeadExtras } from "./lead-lib"
import { validState } from "../lib/hub"

test("parseAuditAt: HH:MM; дефолт 04:30 на пустом; кривое — null (лог + дефолт у демона)", () => {
  expect(parseAuditAt(undefined)).toEqual({ hour: 4, minute: 30 })
  expect(parseAuditAt("  ")).toEqual({ hour: 4, minute: 30 })
  expect(parseAuditAt("5:05")).toEqual({ hour: 5, minute: 5 })
  expect(parseAuditAt("23:59")).toEqual({ hour: 23, minute: 59 })
  expect(parseAuditAt("24:00")).toBeNull()
  expect(parseAuditAt("4:60")).toBeNull()
  expect(parseAuditAt("ночью")).toBeNull()
  expect(parseAuditAt("4-30")).toBeNull()
})

test("localDateKey: локальная дата YYYY-MM-DD с паддингом", () => {
  expect(localDateKey(new Date(2026, 10, 7))).toBe("2026-11-07")
  expect(localDateKey(new Date(2026, 0, 3))).toBe("2026-01-03")
})

test("auditDue: время пришло и сутки не отработаны; повтор в тот же день — нет", () => {
  const at = { hour: 4, minute: 30 }
  const at430 = new Date(2026, 10, 7, 4, 30)
  expect(auditDue(at430, at, undefined)).toBe(true)
  // чуть раньше слота — не пора (граница включительная: 4:30 >= 4:30)
  expect(auditDue(new Date(2026, 10, 7, 4, 29), at, undefined)).toBe(false)
  // сутки уже отработаны (даже вчера-датой) — не стреляем второй раз
  expect(auditDue(at430, at, "2026-11-07")).toBe(false)
  // полдень того же дня после пропуска слота — догон
  expect(auditDue(new Date(2026, 10, 7, 12, 0), at, undefined)).toBe(true)
})

test("auditDue: сутк-ключ берётся из локального времени файра, не из lastFireDate иного дня", () => {
  const at = { hour: 4, minute: 30 }
  const nextDay = new Date(2026, 10, 8, 5, 0)
  expect(auditDue(nextDay, at, "2026-11-07")).toBe(true) // новые сутки — стреляем
})

test("composeAuditPrompt: только метаданные, контракт — в методичке дефа", () => {
  const p = composeAuditPrompt("2026-11-07")
  expect(p).toContain("[ночной аудит]")
  expect(p).toContain("сутки: до 2026-11-07")
  expect(p).toContain("NOTICED BUT NOT TOUCHING")
  expect(p).toContain("ни одного текстового сообщения")
  expect(p).not.toContain("task get") // детали аудита — не в промпте
})

test("validLeadExtras: lastFireDate YYYY-MM-DD персистится, битое стрипится", () => {
  const good = validState({ offset: 0, queue: [], cron: { lastFireDate: "2026-11-07" } }, validLeadExtras)
  expect(good.cron).toEqual({ lastFireDate: "2026-11-07" })
  const junk = validState({ offset: 0, queue: [], cron: { lastFireDate: "07.11.2026" } }, validLeadExtras)
  expect(junk.cron).toBeUndefined()
  const notObj = validState({ offset: 0, queue: [], cron: "вчера" }, validLeadExtras)
  expect(notObj.cron).toBeUndefined()
})
