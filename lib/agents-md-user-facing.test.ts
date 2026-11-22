// agents-md-user-facing.test.ts — пин юзер-facing инварианта перевода
// агентских конфигов (uwuwu#1383). Переводится внутренняя кухня —
// методички, скиллы, промпты, серверные контракты; язык ответов владельцу
// остаётся русским, и его носитель — директива «ALWAYS RESPOND IN RUSSIAN»
// в шапке AGENTS.md (канон, который читает каждый спавн). Директиву снесли
// или переформулировали мимо канона — тест красный до мержа.
// Статический тест: файл читается локально, сети нет (конвенция
// relay-deploy-contract.test.ts).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "..")
const agentsMd = readFileSync(join(ROOT, "AGENTS.md"), "utf8")

describe("AGENTS.md — юзер-facing язык ответов (#1383)", () => {
  test("директива ALWAYS RESPOND IN RUSSIAN на месте: перевод кухни не меняет язык владельцу", () => {
    expect(agentsMd).toContain("ALWAYS RESPOND IN RUSSIAN")
  })
})
