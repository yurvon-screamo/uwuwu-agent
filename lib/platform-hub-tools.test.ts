// platform-hub-tools.test.ts — юнит-тесты чистых функций hub-маппинга
// (bun test, без сети). Живой кейс — инцидент 1070: скан отдаёт ячейку
// Autodone в UI-семантике "✓", consumer демона ждёт легаси-"true" —
// автодон не видел ни одного autodone=true на hub-транспорте.
import { describe, expect, test } from "bun:test"
import { autodoneCellValue, autodoneFmOf, hubTool } from "./platform-hub-tools"

describe("autodoneCellValue (fm → ячейка скана, легаси-семантика checkbox)", () => {
  test("fm true → ячейка 'true' (consumer factory.ts readTasks ждёт 'true')", () => {
    expect(autodoneCellValue("true")).toBe("true")
  })

  test("fm '✓' (ручная правка) → 'true' — паритет cli-парсера", () => {
    expect(autodoneCellValue("✓")).toBe("true")
  })

  test("fm false/пусто/undefined → '' (выключено)", () => {
    expect(autodoneCellValue("false")).toBe("")
    expect(autodoneCellValue("")).toBe("")
    expect(autodoneCellValue(undefined)).toBe("")
  })

  test("мусор в fm → '' (не бросает; паритет cli — точные 'true'|'✓' без trim)", () => {
    expect(autodoneCellValue("yes")).toBe("")
    expect(autodoneCellValue(" true ")).toBe("")
  })
})

describe("autodoneFmOf (ячейка write-пути → fm, round-trip с read)", () => {
  test("'true' и '✓' → fm 'true'", () => {
    expect(autodoneFmOf("true")).toBe("true")
    expect(autodoneFmOf("✓")).toBe("true")
  })

  test("'' и 'false' → fm 'false'", () => {
    expect(autodoneFmOf("")).toBe("false")
    expect(autodoneFmOf("false")).toBe("false")
  })

  test("round-trip: read→write не разворачивает флаг", () => {
    expect(autodoneFmOf(autodoneCellValue("true"))).toBe("true")
    expect(autodoneFmOf(autodoneCellValue(""))).toBe("false")
  })
})

// ——— живой e2e: hub-скан против живой доски uwuwu (гвард UWUWU_E2E=1) ———
// Гвард по культуре репо (прецедент qlty-review.test.ts): без UWUWU_E2E=1
// сразу зелёный (CI не читает живой hub). Живой прогон — чтение доски.
const hubE2E = process.env.UWUWU_E2E === "1"

describe("read_database_cells e2e — живой скан (UWUWU_E2E=1)", () => {
  test.skipIf(!hubE2E)(
    "ячейка Autodone в легаси-семантике ('true'|'') — consumer factory.ts readTasks ждёт 'true' (инцидент 1070)",
    async () => {
      const res = await hubTool<{ rows: Array<{ rowBlockId: string; cells: Record<string, { value: string }> }> }>(
        "read_database_cells",
        { databaseBlockId: "db:uwuwu" },
      )
      expect(res.rows.length).toBeGreaterThan(0)
      for (const row of res.rows) {
        expect(["true", "", undefined]).toContain(row.cells?.Autodone?.value)
        expect(row.cells?.Autodone?.value).not.toBe("✓")
      }
    },
    120_000,
  )

  test.skipIf(!hubE2E)(
    "rows несут непустой title на верхнем уровне — без него readTasks получает '' и фоллбек тела по тайтлу мёртв (баг-пин tester 1070: bodyDoc треков на hub пуст навсегда)",
    async () => {
      const res = await hubTool<{ rows: Array<{ rowBlockId: string; title?: string }> }>(
        "read_database_cells",
        { databaseBlockId: "db:uwuwu" },
      )
      expect(res.rows.length).toBeGreaterThan(0)
      const untitled = res.rows.filter((r) => typeof r.title !== "string" || r.title.trim() === "")
      expect(untitled.map((r) => r.rowBlockId)).toEqual([])
    },
    120_000,
  )

  test.skipIf(!hubE2E)(
    "rows несут привязку linkedDocId issue:<number> — без неё readTasks даёт bodyDoc='' навсегда: комменты/notify/watchdog-fm глухие (баг-пин tester 1070)",
    async () => {
      const res = await hubTool<{
        rows: Array<{ rowBlockId: string; linkedDocId?: string | null }>
      }>("read_database_cells", { databaseBlockId: "db:uwuwu" })
      expect(res.rows.length).toBeGreaterThan(0)
      const unbound = res.rows.filter(
        (r) => r.linkedDocId !== `issue:${r.rowBlockId}`,
      )
      expect(unbound.map((r) => r.rowBlockId)).toEqual([])
    },
    120_000,
  )
})
