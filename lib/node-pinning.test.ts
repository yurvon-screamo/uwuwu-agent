// node-pinning.test.ts — юнит-тесты контракта пиннинга (bun test, без сети:
// resolvePinningDoc не тестируется здесь — интеграционный, гвардится живым
// ядром; чистые parse/validate покрыты полностью, включая обе семантики
// unknown-проекта — прод отказ, песочница пропуск).
import { describe, expect, test } from "bun:test"
import {
  nodeNameFromEnv,
  parsePinningDoc,
  validateFactory,
  validateSingleton,
  type PinningDoc,
} from "./node-pinning"

// Статус-кво топологии ADR-076: kisa — фабрика №1 (uwuwu/origa/foilcap/true-washing),
// redmi — 1xgames; singleton-демоны на kisa; дример нод-локальный (справочно).
const ADR076_MD = `---
title: Node-pinning
---

# Node-pinning

## Фабрики

- kisa: uwuwu, origa, foilcap, true-washing
- redmi: 1xgames

## Одиночные демоны

- triage: kisa
- cron: kisa

## Нод-локальные (справочно, без валидации)

- dreamer: kisa, redmi
`

function adr076Pinning(): PinningDoc {
  return parsePinningDoc(ADR076_MD)
}

describe("parsePinningDoc", () => {
  test("статус-кво ADR-076 парсится во все три секции", () => {
    const doc = parsePinningDoc(ADR076_MD)
    expect(doc.factories).toEqual({
      kisa: ["uwuwu", "origa", "foilcap", "true-washing"],
      redmi: ["1xgames"],
    })
    expect(doc.singles).toEqual({ triage: "kisa", cron: "kisa" })
    expect(doc.local).toEqual({ dreamer: ["kisa", "redmi"] })
  })

  test("регистр и пробелы ключей нормализуются", () => {
    const doc = parsePinningDoc("## Фабрики\n\n-  Kisa : UwUwu, Origa\n")
    expect(doc.factories).toEqual({ kisa: ["uwuwu", "origa"] })
  })

  test("неизвестная секция — throw, молчаливый пропуск прячет опечатку владельца", () => {
    expect(() => parsePinningDoc("## Роутинг\n\n- kisa: uwuwu\n")).toThrow("неизвестная секция")
  })

  test("секция Фабрики отсутствует или пуста — throw", () => {
    expect(() => parsePinningDoc("# Node-pinning\n\nпроза владельца\n")).toThrow("не найдена или пуста")
    expect(() => parsePinningDoc("## Одиночные демоны\n\n- triage: kisa\n")).toThrow("не найдена или пуста")
  })

  test("строка списка без двоеточия игнорируется, секция с одной прозой — throw пустого списка", () => {
    expect(() => parsePinningDoc("## Фабрики\n\nkisa без списка\n")).toThrow()
  })

  test("singles с несколькими нодами — throw (одна строка = одна нода)", () => {
    expect(() => parsePinningDoc("## Одиночные демоны\n\n- triage: kisa, redmi\n## Фабрики\n\n- kisa: uwuwu\n"))
      .toThrow("одна строка = одна нода")
  })

  test("дубликат ноды в Фабриках — throw", () => {
    expect(() => parsePinningDoc("## Фабрики\n\n- kisa: uwuwu\n- kisa: origa\n")).toThrow("дубликат")
  })

  test("пустое значение ноды (пробел после env-подобной записи) — throw пустого списка", () => {
    expect(() => parsePinningDoc("## Фабрики\n\n- kisa:   \n")).toThrow("пустой список")
  })
})

describe("validateFactory", () => {
  test("статус-кво ADR-076: скоуп kisa и redmi зелёный в проде", () => {
    const pinning = adr076Pinning()
    expect(validateFactory(pinning, "kisa", ["uwuwu", "origa", "foilcap", "true-washing"], false)).toEqual([])
    expect(validateFactory(pinning, "redmi", ["1xgames"], false)).toEqual([])
  })

  test("проект, пиненный за другую ноду — отказ и в проде, и в песочнице", () => {
    const pinning = adr076Pinning()
    for (const sandboxed of [false, true]) {
      const violations = validateFactory(pinning, "redmi", ["uwuwu"], sandboxed)
      expect(violations).toHaveLength(1)
      expect(violations[0]).toContain("пинен за ноду «kisa»")
      expect(violations[0]).toContain("пересечение скоупов")
    }
  })

  test("unknown-проект в проде — отказ: конфиг без пина = незакрытая пара нода/проект", () => {
    const violations = validateFactory(adr076Pinning(), "redmi", ["1xgames", "netcore"], false)
    expect(violations).toHaveLength(1)
    expect(violations[0]).toContain("не запинен в доке")
    expect(violations[0]).toContain("netcore")
  })

  test("unknown-проект в песочнице — пропускается (смоук-доски в доке не живут)", () => {
    const violations = validateFactory(adr076Pinning(), "kisa", ["smoke"], true)
    expect(violations).toEqual([])
  })

  test("перепин и unknown одновременно — обе ошибки в списке", () => {
    const pinning = adr076Pinning()
    const violations = validateFactory(pinning, "redmi", ["uwuwu", "netcore"], false)
    expect(violations).toHaveLength(2)
    expect(violations.some((v) => v.includes("пересечение скоупов"))).toBe(true)
    expect(violations.some((v) => v.includes("не запинен"))).toBe(true)
  })

  test("регистр имени ноды и проекта не влияет на сверку", () => {
    expect(validateFactory(adr076Pinning(), " Kisa ", ["UwUwu"], false)).toEqual([])
  })
})

describe("validateSingleton", () => {
  test("своя нода — зелёный", () => {
    expect(validateSingleton(adr076Pinning(), "triage", "kisa")).toEqual([])
    expect(validateSingleton(adr076Pinning(), "cron", "kisa")).toEqual([])
  })

  test("чужая нода — отказ: второй экземпляр на контуре недопустим", () => {
    const violations = validateSingleton(adr076Pinning(), "triage", "redmi")
    expect(violations).toHaveLength(1)
    expect(violations[0]).toContain("пинен за ноду «kisa»")
  })

  test("демона нет в доке — отказ: демон без пина не бывает", () => {
    const violations = validateSingleton(adr076Pinning(), "watcher", "kisa")
    expect(violations).toHaveLength(1)
    expect(violations[0]).toContain("не запинен в доке")
  })
})

describe("nodeNameFromEnv", () => {
  test("env задан — env побеждает, нормализуется", () => {
    expect(nodeNameFromEnv(" Redmi ", "unknown-host")).toBe("redmi")
  })

  test("env пустой/пробельный — hostname lowercase", () => {
    expect(nodeNameFromEnv(undefined, "kisa")).toBe("kisa")
    expect(nodeNameFromEnv("", "Kisa.Local")).toBe("kisa.local")
    expect(nodeNameFromEnv("   ", "kisa")).toBe("kisa")
  })
})
