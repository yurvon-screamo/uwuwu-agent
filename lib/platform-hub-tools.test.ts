// platform-hub-tools.test.ts — юнит-тесты чистых функций hub-маппинга
// (bun test, без сети). Живой кейс — инцидент 1070: скан отдаёт ячейку
// Autodone в UI-семантике "✓", consumer демона ждёт легаси-"true" —
// автодон не видел ни одной галочки на hub-транспорте.
import { describe, expect, test } from "bun:test"
import { autodoneCellValue, autodoneFmOf } from "./platform-hub-tools"

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
