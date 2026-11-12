// ci-postmerge-trigger.test.ts — пин post-merge валидации мастер-коммита
// (задача 20261112124313-cl7zbg4, кейс #221). ci.yml в тестах не
// исполняется: его текст — контракт (конвенция ci-qlty-gate.test.ts).
// Батч-мерджи вносят артефакты, невидимые на PR-ветках: каждый PR-ран
// гоняет merge-коммит со своей копией master, и зелёные по отдельности
// диффы конфликтуют только на мастер-коммите (дубль strictPathPin,
// TS2300 — всплыл красным CI чужого PR #224, hotfix cee4c0c). Без
// push-триггера на master артефакт мастер-коммита никто не валидирует
// до первого следующего чужого PR — снос push-ветки триггера валит пин.
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CI_YML = join(import.meta.dir, "..", ".github", "workflows", "ci.yml")
const content = readFileSync(CI_YML, "utf8")

/** Срез секции `on:` — от ключа до `permissions:` (конец триггер-блока). */
function onBlock(src: string): string {
  const start = src.search(/^on:\s*$/m)
  expect(start).toBeGreaterThanOrEqual(0)
  const end = src.indexOf("permissions:", start)
  expect(end).toBeGreaterThan(start)
  return src.slice(start, end)
}

describe("ci.yml — post-merge валидация мастер-коммита (кейс #221)", () => {
  test("push-триггер на master на месте", () => {
    const block = onBlock(content)
    const pushMatch = block.match(/^ {2}push:\n((?: {4}.*\n?)+)/m)
    expect(pushMatch).not.toBeNull()
    const push = pushMatch?.[1] ?? ""
    expect(push).toContain("branches:")
    expect(push).toMatch(/^\s*- master\s*$/m)
  })

  test("pull_request-триггер жив — push-ран гейтом PR не стал", () => {
    // Гейт branch protection прицеплен именно к PR-ранам (ADR-063):
    // push-ран мастер-коммита чека на PR не даёт, сносить
    // pull_request-триггер «взамен» нельзя — только добавка.
    const block = onBlock(content)
    expect(block).toMatch(/^ {2}pull_request:\s*$/m)
  })

  test("старый PR-only вариант валит этот пин (красно-зелёная фиксура)", () => {
    // Дословный on:-блок до коммита 4850d55 — механика кейса #221:
    // мастер-коммит никто не валидирует, артефакт откладывается до
    // следующего чужого PR.
    const legacy = "name: CI\n\non:\n  pull_request:\n\npermissions:\n  contents: read\n"
    expect(() => {
      const block = onBlock(legacy)
      expect(block).toMatch(/^ {2}push:\n/m)
    }).toThrow()
  })
})
