// tests/platform-hub-tools.test.ts — контрактные юниты hub-шва read_database_cells/
// get_doc_attrs (инцидент 1110): rows несут linkedDocId+title (паритет
// docId-схемы легаси-носителя — тело = issue:<number>), клетка «Запланировано» —
// dateMs локальной минуты (паритет cron-lib formatLocal/minuteStart, иначе
// reconcile-спам), attrs несут легаси-алиасы custom-* (attrsOf крона ждёт
// имена легаси-носителя).
// Чистые функции без gh — стабы не нужны (конвенция #264: тест в tests/).
import { describe, expect, test } from "bun:test"
import { issueAttrs, issueRows, plannedToMs } from "../lib/platform-hub-tools"
import { minuteStart } from "../cron/cron-lib"

const masterBody = [
  "---",
  "status: Вейт",
  "schedule: 0 5 * * *",
  "priority: medium",
  "autodone: true",
  "planned: 2026-11-14 05:00",
  "---",
  "## Инструкция",
  "крути индекс",
].join("\n")

const issue = {
  number: 440,
  title: "Крон: суточный semantic sync",
  body: masterBody,
  state: "open",
}

describe("issueRows: паритет схемы легаси-носителя строки", () => {
  const rows = issueRows([issue as never])
  const row = rows[0]

  test("linkedDocId — issue:<number> (гейт bodyDoc крона проходит)", () => {
    expect(row?.linkedDocId).toBe("issue:440")
  })

  test("title — тайтл issue (человекочитаемые логи/ран-карточки)", () => {
    expect(row?.title).toBe("Крон: суточный semantic sync")
  })

  test("cells — fm-канон (Status Вейт, Расписание, Autodone ✓)", () => {
    expect(row?.cells.Status?.value).toBe("Вейт")
    expect(row?.cells.Расписание?.value).toBe("0 5 * * *")
    expect(row?.cells.Autodone?.value).toBe("✓")
  })
})

describe("клетка «Запланировано»: dateMs локальной минуты", () => {
  const cells = issueRows([issue as never])[0]?.cells

  test("паритет с minuteStart крона — reconcile не спамит", () => {
    const ms = cells?.Запланировано?.dateMs
    expect(ms).toBe(minuteStart(new Date(2026, 10, 14, 5, 0)))
  })

  test("точность локальной TZ: 2026-11-14 05:00 — ровно минута, секунды 0", () => {
    expect(plannedToMs("2026-11-14 01:30")).toBe(new Date(2026, 10, 14, 1, 30).getTime())
  })

  test("пустая/отсутствующая дата — dateMs нет (plannedMs null у крона)", () => {
    expect(plannedToMs("")).toBeNull()
    expect(plannedToMs("   ")).toBeNull()
  })

  test("кривая дата — null (молча не стрелять хуже честного null)", () => {
    expect(plannedToMs("45.13")).toBeNull()
    expect(plannedToMs("13 месяц кривой")).toBeNull()
    expect(plannedToMs("2026-13-99 99:99")).toBeNull() // диапазоны
    expect(plannedToMs("2026-02-30 05:00")).toBeNull() // несуществующая (прокрутка Date)
  })
})

describe("issueAttrs: легаси-алиасы custom-* из fm schedule/agent", () => {
  test("schedule → custom-schedule (attrsOf крона читает имя легаси-носителя)", () => {
    const attrs = issueAttrs(masterBody)
    expect(attrs["custom-schedule"]).toBe("0 5 * * *")
    expect(attrs["custom-schedule"]).toBe(attrs.schedule)
  })

  test("agent → custom-agent; без agent — custom-agent отсутствует", () => {
    const withAgent = issueAttrs("---\nschedule: 6h\nagent: lead\n---\nтело")
    expect(withAgent["custom-agent"]).toBe("lead")
    const withoutAgent = issueAttrs("---\nschedule: 6h\n---\nтело")
    expect(withoutAgent["custom-agent"]).toBeUndefined()
  })

  test("без schedule — custom-schedule отсутствует (null-семантика крона: не крон, тихий скип)", () => {
    const attrs = issueAttrs("просто задача без fm")
    expect(attrs["custom-schedule"]).toBeUndefined()
    expect(attrs.schedule).toBeUndefined()
  })
})

describe("issueRows: тело без fm — дефолты контракта", () => {
  const rows = issueRows([{ number: 7, title: "X", body: "", state: "open" } as never])
  test("Status дефолт Todo, planned без dateMs, linkedDocId жив", () => {
    const row = rows[0]
    expect(row?.linkedDocId).toBe("issue:7")
    expect(row?.cells.Status?.value).toBe("Todo")
    expect(row?.cells.Запланировано?.value).toBe("")
    expect(row?.cells.Запланировано?.dateMs).toBeUndefined()
  })
})
