// agents-md-one-clean-comment.test.ts — пин правила одного чистого комментария
// (uwuwu#1740). Правило живёт в AGENTS.md — каноне, который opencode грузит в
// каждую сессию конвейера; новый тул правки/удаления комментария не заводится,
// значит регрессом считается пропажа или ослабление самой клаузы. Тест
// статический: читает файл локально, сети нет (конвенция соседних пинов).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "..")
const lines = readFileSync(join(ROOT, "AGENTS.md"), "utf8").split(/\r?\n/)
const reportBullet = lines.find((line) => line.startsWith("- reports/questions to the user"))
const oneCleanComment = lines.filter((line) => line.includes("**one clean comment**"))

describe("AGENTS.md — один чистый комментарий на раунд (#1740)", () => {
  test("клауза лежит в существующем пункте о комментариях, а не в новой секции", () => {
    expect(reportBullet).toBeDefined()
    expect(oneCleanComment).toHaveLength(1)
    expect(oneCleanComment[0]).toBe(reportBullet)
    const index = lines.findIndex((line) => line.startsWith("- reports/questions to the user"))
    expect(index).toBeGreaterThanOrEqual(0)
    expect(lines[index + 1]).toContain("- waiting for the user's answer")
  })

  test("состав и перечитка отчёта до одной отправки, пробы — на выделенных строках", () => {
    expect(reportBullet).toContain("compose and reread the whole report before posting")
    expect(reportBullet).toContain("one writing system")
    expect(reportBullet).toContain("no probe or debug lines")
    expect(reportBullet).toContain("probes only on dedicated probe rows")
    expect(reportBullet).toContain("one report = one comment")
  })

  test("после отправки — не более одного исправления reply с пометкой", () => {
    expect(reportBullet).toContain("send one correction as a reply with the marker `Correction:` through `reply_to`")
    expect(reportBullet).toContain("not more than one correction per round")
  })

  test("прежнее правило канала сохранено, текст клаузы без подмены письменности", () => {
    expect(reportBullet).toContain("**the user does not read your session** — all communication only in uwuwu")
    expect(reportBullet).not.toMatch(/[\u3040-\u30ff\u3400-\u9fff\uac00-\ud7af\uf900-\ufaff]/)
    expect(reportBullet).not.toContain("__probe__")
  })
})
