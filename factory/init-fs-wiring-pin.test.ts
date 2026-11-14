// init-fs-wiring-pin.test.ts — регресс-пин бага 1054: инит-фетчеры спавна
// (fetchPersonMarkdown/fetchProjectDocSlugs в factory.ts) ходят в git-клон hub
// напрямую (readFileSync/readdirSync); легаси kernel-резолв (до hub-миграции) —
// вызов notebook_ids_by_names и env-швы UWUWU_PERSON_HPATH/UWUWU_PERSON_NOTEBOOK —
// снесён и не возвращается (hub-фасад это tool-имя не поддерживал, глотающий
// catch давал пустой id → оба контура инжекции молча умирали на каждом спавне).
// Фетчеры не экспортированы (юнит-риг factory.ts — задача 113), пин — по
// исходнику, паттерн греп-гварда debris в tests/ (его маркеры легаси-эндпоинтов
// шов notebook_ids_by_names не ловят — этот пин закрывает класс).
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const FACTORY_TS = readFileSync(join(import.meta.dir, "factory.ts"), "utf8")

/** Срез исходника между якорями функций; пропавший якорь даёт пустой срез —
 *  тест падает по содержимому, а не молчаливо проверяет мусор. */
function section(startMark: string, endMark: string): string {
  const from = FACTORY_TS.indexOf(startMark)
  const to = from === -1 ? -1 : FACTORY_TS.indexOf(endMark, from)
  if (from === -1 || to === -1) return ""
  return FACTORY_TS.slice(from, to)
}

describe("инит-фетчеры спавна — FS-носитель, kernel-шов не возвращается (баг 1054)", () => {
  test("kernel-резолв notebook_ids_by_names снесён", () => {
    expect(FACTORY_TS).not.toContain("notebook_ids_by_names")
  })

  test("env-швы мёртвого резолва снесены", () => {
    expect(FACTORY_TS).not.toContain("UWUWU_PERSON_HPATH")
    expect(FACTORY_TS).not.toContain("UWUWU_PERSON_NOTEBOOK")
  })

  test("оба фетчера инита ходят в клон (FS), а не в mcp-резолв", () => {
    const fetchers = section("async function fetchPersonMarkdown", "async function startTask")
    expect(fetchers).toContain("async function fetchPersonMarkdown")
    expect(fetchers).toContain("async function fetchProjectDocSlugs")
    expect(fetchers).toContain("hubCloneRoot()")
    expect(fetchers).toContain("readFileSync")
    expect(fetchers).toContain("readdirSync")
    expect(fetchers).not.toContain("mcp.tool")
  })
})
