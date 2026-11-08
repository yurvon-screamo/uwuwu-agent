// cron-lib.test.ts — расписания, границы, промпты.
import { describe, expect, test } from "bun:test"
import {
  parseSchedule, fieldMatches, due, isOneShot, isValidTracked, nextFire, cellPatches, formatLocal,
  firePrompt, minuteStart, fireGate,
  bodyRenderIssue, chooseBodyDoc, titleFromBody, pathSafe, stripNonContent,
} from "./cron-lib"

/** Парс с громким сбоем вместо non-null assertion: расписание в тестах
 *  обязано разбираться — иначе тест сам себе врёт. */
const mustParse = (s: string) => {
  const sched = parseSchedule(s)
  if (sched === null) throw new Error(`parseSchedule сломан: ${s}`)
  return sched
}


describe("parseSchedule", () => {
  test("интервалы", () => {
    expect(parseSchedule("5m")).toEqual({ kind: "interval", ms: 300_000 })
    expect(parseSchedule("2h")).toEqual({ kind: "interval", ms: 7_200_000 })
    expect(parseSchedule("1d")).toEqual({ kind: "interval", ms: 86_400_000 })
    expect(parseSchedule("05m")).toEqual({ kind: "interval", ms: 300_000 })
    expect(parseSchedule("0m")).toBeNull()
    expect(parseSchedule("5s")).toBeNull()
  })
  test("at", () => {
    expect(parseSchedule("09:30")).toEqual({ kind: "at", hour: 9, minute: 30 })
    expect(parseSchedule("23:59 01.12")).toEqual({ kind: "at", hour: 23, minute: 59, day: 1, month: 12 })
    expect(parseSchedule("25:00")).toBeNull()
    // диапазоны даты — паритет с CLI-валидацией: иначе «легально» молчал бы вечно
    expect(parseSchedule("09:30 32.01")).toBeNull()
    expect(parseSchedule("09:30 01.13")).toBeNull()
    // ведущие нули даты: TS-regex \d{1,2} отвергает, Rust-парсер тоже
    expect(parseSchedule("09:30 001.12")).toBeNull()
    // поле с ведущим нулём легально на обеих сторонах (\d+ / parse::<u32>)
    expect(parseSchedule("01 * * * *")?.kind).toBe("cron")
  })
  test("cron-выражения", () => {
    expect(parseSchedule("*/30 * * * *")?.kind).toBe("cron")
    expect(parseSchedule("* * * *")).toBeNull()
    expect(parseSchedule("раз в минутку")).toBeNull()
    // шаг 0 и ведущие нули шага — «легальный» вечный ноль: отвергаются
    // на парсинге (Rust: [1-9]-чек, паритет)
    expect(parseSchedule("*/0 * * * *")).toBeNull()
    expect(parseSchedule("*/01 * * * *")).toBeNull()
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
    const s = mustParse("5m")
    expect(due(s, 0, new Date())).toBe(true)
  })
  test("interval: не раньше N", () => {
    const s = mustParse("5m")
    const now = new Date("2026-10-25T12:00:00")
    expect(due(s, now.getTime() - 4 * 60_000, now)).toBe(false)
    expect(due(s, now.getTime() - 5 * 60_000, now)).toBe(true)
  })
  test("cron: минута совпала и не стреляли в эту минуту", () => {
    const s = mustParse("*/30 * * * *")
    const now = new Date("2026-10-25T12:30:00")
    expect(due(s, minuteStart(now) - 60_000, now)).toBe(true)
    expect(due(s, minuteStart(now), now)).toBe(false) // уже стреляли
    const off = new Date("2026-10-25T12:31:00")
    expect(due(s, 0, off)).toBe(false)
  })
  test("at: своя минута и догон, один раз", () => {
    const s = mustParse("09:30")
    expect(due(s, 0, new Date("2026-10-25T09:30:00"))).toBe(true)
    expect(due(s, minuteStart(new Date("2026-10-25T09:30:00")), new Date("2026-10-25T09:30:30"))).toBe(false)
    expect(due(s, 0, new Date("2026-10-25T09:31:00"))).toBe(true) // догон после дауна
    expect(due(s, 0, new Date("2026-10-25T09:29:00"))).toBe(false) // рано
  })
  test("at с датой", () => {
    const s = mustParse("09:30 01.12")
    expect(due(s, 0, new Date("2026-12-01T09:30:00"))).toBe(true)
    expect(due(s, 0, new Date("2026-11-30T09:30:00"))).toBe(false)
  })
  test("isOneShot", () => {
    expect(isOneShot(mustParse("09:30"))).toBe(true)
    expect(isOneShot(mustParse("5m"))).toBe(false)
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
  const master = { title: "Проверка X", schedule: "*/30 * * * *" }
  test("тихий тик: без обязательного отчётного коммента и [DONE]", () => {
    const p = firePrompt(master, "проверь и молчи", "20261107090000-run001", "1xgames")
    expect(p).not.toContain("Доложи результат")
    expect(p).toContain("Здоровый ран — тихий")
    expect(p).toContain("ран-карточка — запись запуска")
    expect(p).not.toContain("[DONE]")
    expect(p).not.toContain("🤖")
  })
  test("ран-карточка названа: агент не работает по ней и статус не меняет", () => {
    const p = firePrompt(master, "", "20261107090000-run001", "uwuwu")
    expect(p).toContain("Ран-карточка этого запуска — задача 20261107090000-run001")
    expect(p).toContain("статус не меняй (закрывает владелец)")
  })
  test("исключение: команда коммента на ран с project и ЧТО РЕШИТЬ", () => {
    const p = firePrompt(master, "=== СЕКРЕТ ===\nпроверь", "20261107090000-run001", "1xgames")
    expect(p).toContain("uwuwu-cli task comment 20261107090000-run001 --project 1xgames")
    expect(p).toContain("ЧТО РЕШИТЬ")
    expect(p).toContain("ТОЛЬКО исключение")
    expect(p).toContain("≡")
  })
  test("блокер: Needs attention на ране тем же ходом", () => {
    const p = firePrompt(master, "", "20261107090000-run001", "uwuwu")
    expect(p).toContain('task edit 20261107090000-run001 --project uwuwu --status "Needs attention"')
  })
  test("ран снят в полёте — завершить ход молча (ревью Common-2/решение владельца)", () => {
    const p = firePrompt(master, "", "20261107090000-run001", "uwuwu")
    expect(p).toContain("заверши ход молча")
  })
  test("тело уходит в промпт как есть — стаба пустого тела нет (гейт не пускает пустое)", () => {
    const p = firePrompt(master, "делай работу", "20261107090000-run001", "uwuwu")
    expect(p).toContain("=== ТЕЛО ЗАДАЧИ ===\nделай работу\n=== КОНЕЦ ТЕЛА ===")
    expect(p).not.toContain("пусто — отработай по заголовку")
  })
})

describe("fireGate: файр только по живой Вейт-строке свежего среза", () => {
  const rows = [
    { rowId: "alive", bodyDoc: "d", title: "T", schedule: "5m", plannedMs: null, scheduleCell: "", status: "вейт" },
    { rowId: "moved", bodyDoc: "d", title: "T", schedule: "5m", plannedMs: null, scheduleCell: "", status: "in progress" },
  ]
  test("строка снята — removed, файра нет", () => {
    expect(fireGate(rows, "gone")).toBe("removed")
  })
  test("строка снята с Вейт после среза — not-wait, файра нет", () => {
    expect(fireGate(rows, "moved")).toBe("not-wait")
  })
  test("живая Вейт-строка — fire", () => {
    expect(fireGate(rows, "alive")).toBe("fire")
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
    const s = mustParse("09:30")
    expect(due(s, 0, new Date("2026-10-25T09:37:12"))).toBe(true)
  })
  test("минута ещё не наступила — нет", () => {
    const s = mustParse("09:30")
    expect(due(s, 0, new Date("2026-10-25T09:29:00"))).toBe(false)
  })
})

describe("bodyRenderIssue: тело, рендерящееся агенту в пустоту", () => {
  test("только HTML-комментарии (unsupported-легаси) → причина", () => {
    const md = "<!-- unsupported: flavour=affine:database -->\n<!-- вторая легаси-строка -->\n"
    expect(bodyRenderIssue(md)).toBe("тело рендерится в пусто/unsupported-комментарии")
  })
  test("живой экспорт-шейп инцидента: frontmatter + заголовок-эхо + комментарии → причина", () => {
    const md = "---\ntitle: крон\n---\n\n# крон\n\n<!-- unsupported: flavour=affine:database -->\n<!-- e2e: рендер в пустоту -->\n"
    expect(bodyRenderIssue(md)).not.toBeNull()
  })
  test("frontmatter + комментарии + zero-width → причина", () => {
    const md = "---\ntitle: x\n---\n# x\n<!-- коммент -->\n\u200B\uFEFF"
    expect(bodyRenderIssue(md)).not.toBeNull()
  })
  test("пустая строка → причина", () => {
    expect(bodyRenderIssue("")).not.toBeNull()
    expect(bodyRenderIssue("  \n\n")).not.toBeNull()
  })
  test("av-экспорт доски (таблица) читаем — валиден", () => {
    const md = "# Доска\n\n| Заголовок | Status |\n| --- | --- |\n| строка | active |\n"
    expect(bodyRenderIssue(md)).toBeNull()
  })
  test("текст за комментариями виден", () => {
    expect(bodyRenderIssue("<!-- коммент -->\nнастоящая постановка")).toBeNull()
  })
})

describe("stripNonContent", () => {
  test("frontmatter и заголовок-эхо срезаются только в начале дока", () => {
    expect(stripNonContent("---\ntitle: t\n---\n# эхо\nтекст")).toBe("текст")
    expect(stripNonContent("текст\n# внутренний заголовок")).toContain("внутренний заголовок")
  })
  test("frontmatter не в начале — контент", () => {
    expect(stripNonContent("текст\n---\nне frontmatter\n---\n")).toContain("не frontmatter")
  })
})

describe("chooseBodyDoc: паритет CLI choose_body_doc", () => {
  test("живая привязка → она, заголовок не смотрим", () => {
    expect(chooseBodyDoc("doc-bound", true, ["doc-a", "doc-b"])).toEqual({ kind: "doc", docId: "doc-bound" })
  })
  test("мёртвая привязка → title-fallback на единственный док", () => {
    expect(chooseBodyDoc("doc-dead", false, ["doc-a"])).toEqual({ kind: "doc", docId: "doc-a" })
  })
  test("detached без доков → missing", () => {
    expect(chooseBodyDoc("", false, [])).toEqual({ kind: "missing" })
  })
  test("дубли тел → ошибка со списком", () => {
    expect(chooseBodyDoc("doc-dead", false, ["doc-a", "doc-b"])).toEqual({ kind: "duplicates", docIds: ["doc-a", "doc-b"] })
  })
})

describe("titleFromBody", () => {
  test("заголовок-эхо и unsupported-мусор в имя не попадают", () => {
    const md = "---\ntitle: метадата\n---\n# эхо\n<!-- unsupported -->\n## Реальная секция\nтекст"
    expect(titleFromBody(md)).toBe("Реальная секция")
  })
  test("без заголовка — первая содержательная строка", () => {
    expect(titleFromBody("первая строка\nвторая")).toBe("первая строка")
  })
  test("пусто/мусор → пустая строка", () => {
    expect(titleFromBody("# эхо\n<!-- only comments -->\u200B")).toBe("")
    expect(titleFromBody("")).toBe("")
  })
})

describe("pathSafe", () => {
  test("'/' → U+2215 (конвенция CLI path_safe)", () => {
    expect(pathSafe("RU/EN слэш")).toBe("RU∕EN слэш")
    expect(pathSafe("без слэша")).toBe("без слэша")
  })
})

describe("isValidTracked: lastRefusal", () => {
  test("опциональная строка — валидна", () => {
    expect(isValidTracked({ sessionID: "", lastFiredAt: 5, firedAtIso: "x", lastRefusal: "причина" })).toBe(true)
  })
  test("старые записи без поля — валидны (обратная совместимость state)", () => {
    expect(isValidTracked({ sessionID: "ses_1", lastFiredAt: 5, firedAtIso: "x" })).toBe(true)
  })
  test("не-строка — битая запись", () => {
    expect(isValidTracked({ sessionID: "", lastFiredAt: 5, lastRefusal: 42 })).toBe(false)
  })
})


describe("nextFire: следующее планируемое срабатывание", () => {
  const at = (y: number, m: number, d: number, h: number, min: number, s = 0) => new Date(y, m - 1, d, h, min, s)

  test("интервал — от последнего файра", () => {
    const sched = mustParse("6h")
    const last = at(2026, 11, 8, 2, 34, 21)
    expect(nextFire(sched, last)).toEqual(at(2026, 11, 8, 8, 34))
  })

  test("cron-выражение — следующий совпавший минут-старт", () => {
    const sched = mustParse("*/15 * * * *")
    const last = at(2026, 11, 8, 10, 3)
    expect(nextFire(sched, last)).toEqual(at(2026, 11, 8, 10, 15))
  })

  test("cron-выражение с днём недели — не стреляет в прошедшие дни повторно", () => {
    const sched = mustParse("0 9 * * 1")
    // 08.11.2026 — воскресенье: следующий понедельник 09.11
    const last = at(2026, 11, 8, 9, 0)
    expect(nextFire(sched, last)).toEqual(at(2026, 11, 9, 9, 0))
  })

  test("разовый HH:MM — сегодняшний момент, если он впереди", () => {
    const sched = mustParse("18:00")
    const last = at(2026, 11, 8, 10, 0)
    expect(nextFire(sched, last)).toEqual(at(2026, 11, 8, 18, 0))
  })

  test("разовый в прошлом → null (нечего планировать)", () => {
    const sched = mustParse("09:30")
    const last = at(2026, 11, 8, 10, 0)
    expect(nextFire(sched, last)).toBeNull()
  })

  test("разовый с датой — конкретный день, месяц из расписания", () => {
    const sched = mustParse("09:30 01.12")
    const last = at(2026, 11, 8, 10, 0)
    expect(nextFire(sched, last)).toEqual(at(2026, 12, 1, 9, 30))
  })

  test("вечный молчун (ни одного совпадения за год) → null, клетку не трогает", () => {
    const sched = mustParse("0 0 30 2 *")
    const last = at(2026, 11, 8, 10, 0)
    expect(nextFire(sched, last)).toBeNull()
  })
})

describe("cellPatches: расхождение клеток мастера", () => {
  const at = (y: number, m: number, d: number, h: number, min: number) => new Date(y, m - 1, d, h, min)
  const row = (over: Record<string, unknown> = {}) => ({
    rowId: "row-1", bodyDoc: "doc-1", title: "мастер", schedule: "6h",
    plannedMs: null, scheduleCell: "", status: "вейт", ...over,
  })
  const sched = mustParse("6h")

  test("чистая доска — пустой патч (записей не нужно)", () => {
    const r = row({ plannedMs: at(2026, 11, 8, 8, 34).getTime(), scheduleCell: "6h" })
    expect(cellPatches(r, sched, at(2026, 11, 8, 2, 34).getTime(), at(2026, 11, 8, 3, 0))).toEqual({})
  })

  test("пустая клетка planned при живом файре — патч planned", () => {
    const r = row({ scheduleCell: "6h" })
    const patch = cellPatches(r, sched, at(2026, 11, 8, 2, 34).getTime(), at(2026, 11, 8, 3, 0))
    expect(patch).toEqual({ planned: "2026-11-08 08:34" })
  })

  test("дрейф клетки «Расписание» — патч schedule значением attr", () => {
    const r = row({ plannedMs: at(2026, 11, 8, 8, 34).getTime(), scheduleCell: "" })
    const patch = cellPatches(r, sched, at(2026, 11, 8, 2, 34).getTime(), at(2026, 11, 8, 3, 0))
    expect(patch).toEqual({ schedule: "6h" })
  })

  test("never-fired cron + пустая клетка — гард: без вычисления next (вечный молчун не сканит год каждым тиком)", () => {
    const cronSched = mustParse("*/15 * * * *")
    const r = row({ schedule: "*/15 * * * *", scheduleCell: "*/15 * * * *" })
    const patch = cellPatches(r, cronSched, 0, at(2026, 11, 8, 10, 0))
    expect(patch).toEqual({})
  })

  test("never-fired interval — плановое время now (due файрит следующим тиком, не now+interval)", () => {
    const r = row({ scheduleCell: "6h" })
    const patch = cellPatches(r, sched, 0, at(2026, 11, 8, 10, 0))
    expect(patch).toEqual({ planned: "2026-11-08 10:00" })
  })

  test("never-fired at — момент HH:MM без гарда (O(1), доска отвечает сразу)", () => {
    const atSched = mustParse("18:00")
    const r = row({ schedule: "18:00", scheduleCell: "18:00" })
    const patch = cellPatches(r, atSched, 0, at(2026, 11, 8, 10, 0))
    expect(patch).toEqual({ planned: "2026-11-08 18:00" })
  })

  test("never-fired interval с ненулевой клеткой — сходится к now (легаси/ручная дата не переживает семантику due)", () => {
    const r = row({ plannedMs: at(2026, 11, 1, 8, 0).getTime(), scheduleCell: "6h" })
    const patch = cellPatches(r, sched, 0, at(2026, 11, 8, 10, 0))
    expect(patch).toEqual({ planned: "2026-11-08 10:00" })
  })
})

describe("formatLocal", () => {
  test("минутная точность с ведущими нулями", () => {
    expect(formatLocal(new Date(2026, 10, 8, 2, 34))).toBe("2026-11-08 02:34")
    expect(formatLocal(new Date(2026, 0, 5, 9, 5))).toBe("2026-01-05 09:05")
  })
})
