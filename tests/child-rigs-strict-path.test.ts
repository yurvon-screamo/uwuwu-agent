// tests/child-rigs-strict-path.test.ts — страж класса тихого маскирования
// PR #207 в интеграционных ригах: каждый tests/*-child.ts с opencode-моком
// обязан быть на строгом pathname-матче (сканер — child-rigs-scan.ts) и нести
// регресс-пин strictPathPin. Мьют-тест держит сам сканер острым: каждый класс
// маскирования обязан краснить, легальные паттерны — не краснить.
import { describe, expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { scanRig } from "./child-rigs-scan"

const TESTS_DIR = import.meta.dir

describe("страж класса PR #207: интеграционные риги на строгом pathname-матче", () => {
  const rigs = readdirSync(TESTS_DIR).filter((f) => f.endsWith("-child.ts"))

  test("каждый *-child риг без loose-матчей и с пином", () => {
    expect(rigs.length).toBeGreaterThan(10)
    const report: string[] = []
    for (const rig of rigs) {
      const violations = scanRig(readFileSync(join(TESTS_DIR, rig), "utf-8"))
      for (const v of violations) report.push(`${rig}:${v}`)
    }
    expect(report).toEqual([])
  })

  test("страж краснеет на каждом классе маскирования (мьюты)", () => {
    const mutants: Array<[string, string]> = [
      ["endsWith-класс", `if (url.endsWith("/api/session") && init?.method === "POST") {`],
      ["includes-класс", `if (url.includes("/api/session/")) {`],
      ["bare-суффикс", `if (url.includes("/prompt")) {`],
      ["безъякорный regex", `if (/\\/api\\/session\\/(ses_[^/]+)\\/prompt$/.test(url)) {`],
      ["slice-нормализация", `const path = url.pathname.startsWith("/api") ? url.pathname.slice("/api".length) : url.pathname`],
      ["риг без пина", `if (path === "/api/session") { return json({ id: "ses_new" }) }`],
    ]
    for (const [label, line] of mutants) {
      const base = `const path = new URL(url).pathname\nif (path === "/api/model") return json({})\nconst strictPathPin = true\n`
      const withMock = label === "риг без пина" ? line : `${line}\n${base}`
      const violations = scanRig(withMock)
      expect(violations.length).toBeGreaterThan(0)
    }
  })

  test("легальные паттерны не краснят страж (SiYuan-маты, якорные regex, pathname-равенства)", () => {
    const legal = [
      `if (url.includes("/api/attr/getBlockAttrs")) {`,
      `if (url.includes("/api/block/appendBlock")) {`,
      `if (/^\\/api\\/session\\/(ses_[^/]+)\\/prompt$/.test(path)) {`,
      `if (path === "/api/model") {`,
      `// fetch на http://mock.invalid/api/api/session — мок обязан бросить`,
      `const strictPathPin = await fetch("http://mock.invalid/api/api/session", { method: "POST" })`,
    ].join("\n")
    expect(scanRig(legal)).toEqual([])
  })
})
