// incident-registry-slug.test.ts — пин полного слага реестра инцидентов в
// методичке дримера (uwuwu#1622). Голый слаг `platform/…` на write-путях MCP
// уходит в файл вне структуры вики (семья #1219/#1575, гард #1613), а на
// read-пути просто не открывается: живой реестр лежит по
// `Experience/platform/…`. Тест статический: файл читается локально, сети
// нет (конвенция agents-md-user-facing.test.ts).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "..")
const dreamer = readFileSync(join(ROOT, "agents", "dreamer.md"), "utf8")

const FULL_SLUG = "Experience/platform/Инцидент-реестр конвейера"
const BARE_SLUG = /(?<!Experience\/)platform\/Инцидент-реестр конвейера/

describe("dreamer.md — полный слаг реестра инцидентов (#1622)", () => {
  test("обе точки (чтение и создание) несут полный слаг", () => {
    expect(dreamer.split(FULL_SLUG).length - 1).toBeGreaterThanOrEqual(2)
  })

  test("голого слага реестра в методичке нет", () => {
    expect(dreamer).not.toMatch(BARE_SLUG)
  })
})
