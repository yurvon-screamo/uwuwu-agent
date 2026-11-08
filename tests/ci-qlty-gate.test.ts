// ci-qlty-gate.test.ts — контент-пин qlty-шага CI (bun test, без внешних
// процессов). Гейт обязан гонять строковую дельту (`check --upstream
// origin/master`), а не файл-скоуп прогон: файловая обёртка красит PR
// pre-existing baseline-долгом тронутого файла (инцидент PR #158, задача
// 20261108060615-hjw3uqp). Юниты тула qlty_review этот регресс не ловят —
// ci.yml не исполняется в тестах, его текст и есть контракт.
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CI_YML = join(import.meta.dir, "..", ".github", "workflows", "ci.yml")

/** Строки-запуски `qlty check` (не комментарии, не имена шагов, не установка тула). */
function qltyCheckLines(content: string): string[] {
  return content
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => !l.startsWith("#") && !l.startsWith("- name:") && /qlty[^\w]*check\b/.test(l))
    .filter((l) => !l.includes("curl"))
}

describe("CI qlty-гейт — строковая дельта (анти-регресс PR #158)", () => {
  test("qlty check в ci.yml гоняет --upstream origin/master (--no-fix на месте)", () => {
    const content = readFileSync(CI_YML, "utf8")
    const checks = qltyCheckLines(content)
    expect(checks.length).toBeGreaterThanOrEqual(1)
    for (const line of checks) {
      expect(line).toContain("--upstream origin/master")
      expect(line).toContain("--no-fix")
    }
  })

  test("git-diff файловая обёртка не вернулась, fetch-depth: 0 сохранён", () => {
    const content = readFileSync(CI_YML, "utf8")
    // Обёртка строила список путей для файл-скоуп прогона — на строковой
    // дельте qlty сам детектит изменённые пути, обёртке места нет.
    expect(content).not.toContain("--diff-filter")
    expect(content).not.toContain("--name-only")
    // origin/master в Actions существует только при полном чекауте.
    expect(content).toContain("fetch-depth: 0")
  })

  test("старая файловая обёртка валит этот пин (красно-зелёная фиксура)", () => {
    // Дословный qlty-шаг с merge-base b7f3533 (механика до PR #179).
    const legacy = `
      - name: qlty check (delta)
        run: |
          files=$(git diff --name-only --diff-filter=d origin/master...HEAD)
          if [ -z "$files" ]; then
            echo "Пустая дельта — qlty-прогон не требуется"
            exit 0
          fi
          printf '%s\\n' "$files" | xargs -r -d '\\n' "$HOME/.qlty/bin/qlty" check --no-fix
`
    const checks = qltyCheckLines(legacy)
    expect(checks.length).toBe(1)
    expect(checks[0]).not.toContain("--upstream")
    expect(() => {
      expect(checks[0]).toContain("--upstream origin/master")
    }).toThrow()
  })
})
