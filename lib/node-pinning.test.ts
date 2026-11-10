// node-pinning.test.ts — юнит-тесты контракта пиннинга (bun test, без сети:
// resolvePinningDoc не тестируется здесь — интеграционный, гвардится живым
// ядром; чистые parse/validate покрыты полностью, включая обе семантики
// unknown-проекта — прод отказ, песочница пропуск).
import { describe, expect, test } from "bun:test"
import {
  checkNodePinning,
  failPinnedStart,
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

  test("zero-width артефакты ядра (ZWSP в заголовке секции) не ломают парс", () => {
    const doc = parsePinningDoc("## Фабрики\u200b\n\n- kisa: uwuwu\n")
    expect(doc.factories).toEqual({ kisa: ["uwuwu"] })
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

// ——— checkNodePinning: контракт входа и сквозной резолв (mock-транспорт) ———

/** Mock-транспорт: организ-дерево с доком «Node-pinning» в uwuwu и тёзкой в
 *  другой папке (не источник истины). Не-«Node-pinning» вызовы — throw
 *  (тест упадёт громко, не молча вернув пустоту). */
function mockPinningTool(docTitle = "Node-pinning", exists = true): Parameters<typeof checkNodePinning>[0] {
  const nodes = [
    { id: "folder-uwuwu", type: "folder", data: "uwuwu", parentId: "" },
    { id: "folder-other", type: "folder", data: "other-proj", parentId: "" },
    { id: "doc-pinning", type: "doc", data: "docid-pinning", parentId: "folder-uwuwu" },
    { id: "doc-twin", type: "doc", data: "docid-twin", parentId: "folder-other" },
  ]
  return async (name, args) => {
    if (name === "list_organize_nodes") return { nodes }
    if (name === "get_doc") {
      const docId = (args as { docId?: string }).docId
      if (docId === "docid-pinning") return { title: docTitle }
      if (docId === "docid-twin") return { title: docTitle }
      return { title: "другой док" }
    }
    if (name === "export_doc_markdown") {
      if ((args as { docId?: string }).docId === "docid-pinning" && exists) return { exists: true, markdown: ADR076_MD }
      return { exists: false }
    }
    throw new Error(`mock: неожиданный вызов ${name}`)
  }
}

describe("checkNodePinning", () => {
  test("ни одного поля скоупа — throw: вызов молча ничего бы не проверил", async () => {
    expect(checkNodePinning(mockPinningTool(), { hostname: "kisa" })).rejects.toThrow("ровно одно из singletonDaemon/factoryProjects")
  })

  test("оба поля скоупа — throw: иначе factoryProjects молча игнорируется", async () => {
    expect(checkNodePinning(mockPinningTool(), {
      singletonDaemon: "triage",
      factoryProjects: ["uwuwu"],
      hostname: "kisa",
    })).rejects.toThrow("получено 2")
  })

  test("сквозняк: док резолвится из папки uwuwu, тёзка в другой папке игнорируется", async () => {
    const check = await checkNodePinning(mockPinningTool(), {
      factoryProjects: ["uwuwu", "origa", "foilcap", "true-washing"],
      sandboxed: false,
      hostname: "kisa",
    })
    expect(check.nodeName).toBe("kisa")
    expect(check.violations).toEqual([])
  })

  test("тёзка заголовка в папке uwuwu пуста (exists:false) — отказ, тёзка вне папки не спасает", async () => {
    expect(checkNodePinning(mockPinningTool("Node-pinning", false), {
      singletonDaemon: "triage",
      hostname: "kisa",
    })).rejects.toThrow("не экспортируется")
  })

  test("папка проекта uwuwu отсутствует в дереве — отказ", async () => {
    const emptyTree = async (_name: string, args: Record<string, unknown>) => {
      if (_name === "list_organize_nodes") return { nodes: [] }
      throw new Error(`mock: неожиданный вызов ${_name} ${JSON.stringify(args)}`)
    }
    expect(checkNodePinning(emptyTree, { singletonDaemon: "triage", hostname: "kisa" })).rejects.toThrow("нет папки проекта")
  })
})

describe("failPinnedStart", () => {
  test("непустые violations — отказ в print и exit 1", () => {
    const lines: string[] = []
    const exits: number[] = []
    const originalExit = process.exit
    process.exit = ((code?: number) => {
      exits.push(code ?? 0)
      return undefined as never
    }) as typeof process.exit
    try {
      failPinnedStart({ nodeName: "redmi", violations: ["проект «uwuwu» пинен за kisa"], sandboxSkipped: [] }, (line) => lines.push(line))
    } finally {
      process.exit = originalExit
    }
    expect(exits).toEqual([1])
    expect(lines).toHaveLength(1)
    expect(lines[0]).toContain("отказ старта (нода redmi)")
    expect(lines[0]).toContain("пинен за kisa")
  })

  test("пустые violations — no-op (демон продолжает свои строки подтверждения)", () => {
    let printed = false
    let exited = false
    const originalExit = process.exit
    process.exit = (() => {
      exited = true
      return undefined as never
    }) as typeof process.exit
    try {
      failPinnedStart({ nodeName: "kisa", violations: [], sandboxSkipped: [] }, () => {
        printed = true
      })
    } finally {
      process.exit = originalExit
    }
    expect(printed).toBe(false)
    expect(exited).toBe(false)
  })
})
