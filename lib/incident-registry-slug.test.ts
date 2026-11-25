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

// Переносы строк в markdown не несут смысла — сводим пробелы, чтобы ловить
// формулировку каждой точки, а не обёртку строки (конвенция
// roles-split-contract.test.ts). Каждая точка пинится своим локальным
// контекстом: счётчик вхождений по файлу ослабляется третьим упоминанием.
function flat(s: string): string {
  return s.replace(/\s+/g, " ")
}

const body = flat(dreamer)
const FULL_SLUG = "Experience/platform/Инцидент-реестр конвейера"
const BARE_SLUG = /(?<!Experience\/)platform\/Инцидент-реестр конвейера/

describe("dreamer.md — полный слаг реестра инцидентов (#1622)", () => {
  test("read-указатель несёт полный слаг", () => {
    expect(body).toContain(`(the slug \`${FULL_SLUG}\`; if the read did not match`)
  })

  test("create-инструкция несёт полный слаг", () => {
    expect(body).toContain(`No doc — create one (\`uwuwu_docs_write\` with the slug \`${FULL_SLUG}\`;`)
  })

  test("голого слага реестра в методичке нет", () => {
    expect(dreamer).not.toMatch(BARE_SLUG)
  })
})
