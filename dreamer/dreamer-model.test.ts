// dreamer-model.test.ts — пин боевой модели дремоты: константа спавна демона
// и канон-деф агента должны совпадать.
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"
import { parseAgentDefModel } from "../cron/cron-lib"

/** Пин боевой модели дремоты. Решение владельца 11.11 — перевод дремоты на
 *  glm-5.3-flash#high (задача 20261110180337-xnt3j92: K1, спайк-замер
 *  токен-затрат; «Переводи дримера на флеш high, ревьювера оставь как есть»).
 *  Слом фронта = тихая смена модели пост-обработки: демон спавнит дочерние
 *  сессии с моделью из константы MODEL (fireDreamer → POST /session), модель
 *  сессии после спавна никто не перечитывает. Импортировать dreamer.ts нельзя
 *  (void main() безусловный — это исполняемый демон), поэтому пин — по
 *  исходнику (CI-safe, как пин канон-дефа в cron-lib.test.ts). */
const EXPECTED_DREAM_MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash", variant: "high" }

function parseSpawnModelConst(src: string): { providerID: string; id: string; variant: string } | null {
  const body = /const MODEL = (\{[^}]*\}) as const/.exec(src)?.[1]
  if (body === undefined) return null
  const field = (name: string): string | undefined =>
    new RegExp(`${name}:\\s*"([^"]+)"`).exec(body)?.[1]
  const providerID = field("providerID")
  const id = field("id")
  const variant = field("variant")
  if (providerID === undefined || id === undefined || variant === undefined) return null
  return { providerID, id, variant }
}

describe("пин модели дремоты — константа спавна демона и канон-деф согласованы", () => {
  test("константа MODEL демона — боевая модель спавна дочерних сессий", () => {
    const path = join(import.meta.dir, "dreamer.ts")
    expect(existsSync(path)).toBe(true)
    expect(parseSpawnModelConst(readFileSync(path, "utf-8"))).toEqual(EXPECTED_DREAM_MODEL)
  })

  test("канон-деф dreamer — та же модель, что спавнит демон (рассинхрон = красный)", () => {
    // Два механизма спавна дремоты — демон (константа MODEL) и крон
    // «Дример дримеров» (файр-гейт читает frontmatter дефа) — обязаны ехать
    // на одной модели: перевод дремоты меняет оба места разом. Детальный пин
    // дефа — в cron-lib.test.ts; здесь — только консистентность пары.
    const defPath = join(import.meta.dir, "..", "agents", "dreamer.md")
    expect(existsSync(defPath)).toBe(true)
    expect(parseAgentDefModel(readFileSync(defPath, "utf-8"))).toEqual(EXPECTED_DREAM_MODEL)
  })
})
