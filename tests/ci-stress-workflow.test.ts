// ci-stress-workflow.test.ts — пины стресс-джобы (stress-tests.yml, задача
// 20261110051831-qnkx3id). Workflow в тестах не исполняется: его текст —
// контракт (конвенция ci-qlty-gate.test.ts), кроме центральной идиомы —
// цикл прогонов исполняется поведенчески через `bash -ec` с фейковым
// cargo, ровно как это делает раннер GitHub (дефолтный shell шага —
// bash -e; pipefail включён телом шага).
//
// Закреплённые инциденты ревью PR #209:
// - High раунда 1: голый `cargo test | tee` под bash -e + pipefail убивал
//   шаг до записи .rc — «безымянный фейк». Лов `|| rc=$?` — пин.
// - Common раунда 1: AND-OR тернарник врал для stress=false (GitHub
//   expressions возвращают операнды) — вложенная форма — пин.
// - Low раунда 2 → фикс раунда 3: runs=00 проходил валидацию и давал
//   тихий false-green (`[ 0 -ne 00 ]` молчит) — нормализация $((10#…)) — пин.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { afterAll, describe, expect, test } from "bun:test"

const ROOT = join(import.meta.dir, "..")
const WF = join(ROOT, ".github", "workflows", "stress-tests.yml")
const CI_YML = join(ROOT, ".github", "workflows", "ci.yml")
const content = readFileSync(WF, "utf8")
const ciContent = readFileSync(CI_YML, "utf8")

const tmpRoots: string[] = []

afterAll(() => {
  for (const dir of tmpRoots) rmSync(dir, { recursive: true, force: true })
})

/** Тело `run: |` блока шага с данным именем (дедент на ширину блока). */
function stepBody(name: string): string {
  const lines = content.split("\n")
  const nameIdx = lines.findIndex((l) => l.trim() === `- name: ${name}`)
  expect(nameIdx).toBeGreaterThanOrEqual(0)
  let start = -1
  let indent = ""
  for (let i = nameIdx + 1; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*)run: \|$/)
    if (m) {
      start = i + 1
      indent = m[1]
      break
    }
    if (/^\s*-\s+name:/.test(lines[i])) break // у шага нет run-блока
  }
  expect(start).toBeGreaterThan(nameIdx)
  const body: string[] = []
  for (let i = start; i < lines.length; i++) {
    if (/^\s*-\s+name:/.test(lines[i]) || /^\s*working-directory:/.test(lines[i])) break
    body.push(lines[i].slice(indent.length))
  }
  return body.join("\n")
}

describe("stress-tests.yml — триггеры и права", () => {
  test("PR-гейт не расширен: pull_request только по своим файлам", () => {
    // Path-фильтр держит рутину чистой: на обычных PR джоба не гоняется,
    // чекраны branch protection живут в ci.yml.
    const prBlock = content.slice(content.indexOf("pull_request:"), content.indexOf("permissions:"))
    expect(prBlock).toContain(".github/workflows/stress-tests.yml")
    expect(prBlock).toContain("scripts/ci/**")
    expect(prBlock).not.toMatch(/^\s*- '\*\*'/m)
  })

  test("permissions минимальны: contents: read, ничего сверх", () => {
    const m = content.match(/permissions:\n((?:\s+\w+.*\n)+)/)
    expect(m).not.toBeNull()
    const perms = m?.[1] ?? ""
    expect(perms).toContain("contents: read")
    expect(perms).not.toMatch(/^\s+\w+:\s*(?!read$)write|deployments|packages|pull-requests/m)
  })

  test("триггеры: schedule + workflow_dispatch присутствуют", () => {
    expect(content).toContain("schedule:")
    expect(content).toContain("workflow_dispatch:")
    expect(content).toMatch(/cron: '45 18/)
  })
})

describe("stress-tests.yml — supply-chain пины", () => {
  test("все actions запины по SHA (40 hex), плавающих тегов нет", () => {
    const uses = [...content.matchAll(/^\s*- uses: (\S+?)(@\S+)?\s*(#.*)?$/gm)]
    expect(uses.length).toBeGreaterThanOrEqual(3)
    for (const ref of uses.map((m) => m[2])) {
      expect(ref).toMatch(/^@[0-9a-f]{40}$/)
      expect(ref).not.toMatch(/^@v\d+/)
    }
  })

  test("rust-toolchain и rust-cache — те же пины, что в ci.yml", () => {
    for (const action of ["dtolnay/rust-toolchain@", "Swatinem/rust-cache@"]) {
      const pin = (s: string) => s.match(new RegExp(`${action}([0-9a-f]{40})`))
      const wfPin = pin(content)?.[1]
      const ciPin = pin(ciContent)?.[1]
      expect(wfPin).toBeDefined()
      expect(ciPin).toBeDefined()
      expect(wfPin).toBe(ciPin)
    }
  })
})

describe("stress-tests.yml — цикл прогонов (контент)", () => {
  test("лов статуса через || rc=$? — High раунда 1 не вернулся", () => {
    const body = stepBody("cargo test ×N")
    expect(body).toContain("cargo test 2>&1 | tee")
    expect(body).toContain('|| rc=$?')
    expect(body).toMatch(/echo "\$rc" > "\.\.\/stress-logs\/run-\$i\.rc"/)
  })

  test("гард полноты .rc: зелёный без всех rc-файлов невозможен", () => {
    const body = stepBody("cargo test ×N")
    expect(body).toMatch(/find \.\.\/stress-logs -name 'run-\*\.rc' \| wc -l/)
    expect(body).toContain('done_runs" -ne "$STRESS_RUNS"')
    expect(body).toContain("exit 1")
  })

  test("валидация runs: не-число/0/00 — громкий фейл, нормализация 10#", () => {
    // Пин фикса runs=00: «00» проходит case («0» ловится только литералом),
    // seq 1 00 пуст, `[ 0 -ne 00 ]` молчит → тихий false-green без
    // нормализации $((10#…)).
    const body = stepBody("cargo test ×N")
    expect(body).toMatch(/case "\$STRESS_RUNS" in ''\|\*\[!0-9\]\*\)/)
    expect(body).toContain("$((10#$STRESS_RUNS))")
    expect(body).toContain('-lt 1')
    expect(body).toContain("::error::STRESS_RUNS")
  })

  test("STRESS_CPU — вложенная форма: stress=false не игнорируется", () => {
    // Пин Common раунда 1: `x && 'true' || 'true'` врал для false.
    expect(content).toContain("(inputs.stress && 'true' || 'false')")
    expect(content).not.toContain("inputs.stress || 'true'")
  })

  test("уборка воркеров на красном пути: Kill CPU load с if: always()", () => {
    const killIdx = content.indexOf("- name: Kill CPU load")
    const killBlock = content.slice(killIdx, content.indexOf("- name: Failure report"))
    expect(killBlock).toContain("if: always() && env.STRESS_CPU == 'true'")
    expect(killBlock).toContain("xargs -r kill")
  })

  test("Failure report: только на failure, markdown в summary, скрипт существует", () => {
    const reportBlock = content.slice(content.indexOf("- name: Failure report"))
    expect(reportBlock).toContain("if: failure()")
    expect(reportBlock).toContain('tee "$GITHUB_STEP_SUMMARY"')
    expect(reportBlock).toContain("scripts/ci/stress-report.sh stress-logs")
    expect(() => readFileSync(join(ROOT, "scripts", "ci", "stress-report.sh"))).not.toThrow()
  })
})

describe("цикл прогонов — поведенческий тест bash -ec", () => {
  /**
   * Исполняет тело шага «cargo test ×N» ровно как раннер GitHub:
   * bash -ec (дефолтный shell шага), cargo подменён stub'ом на PATH.
   * Возвращает rc-файлы прогонов и exit-код шага.
   */
  function runStepBody(body: string, stubCargo: string): { exit: number; rcFiles: string[] } {
    const root = mkdtempSync(join(tmpdir(), "stress-loop-test-"))
    tmpRoots.push(root)
    const cliDir = join(root, "cli")
    const binDir = join(root, "bin")
    const { mkdirSync, readdirSync } = require("node:fs") as typeof import("node:fs")
    mkdirSync(cliDir, { recursive: true })
    mkdirSync(binDir, { recursive: true })
    writeFileSync(join(binDir, "cargo"), stubCargo, { mode: 0o755 })
    const proc = Bun.spawnSync(["bash", "-ec", body], {
      cwd: cliDir,
      env: { ...process.env, STRESS_RUNS: "3", PATH: `${binDir}:${process.env.PATH}` },
    })
    // Шаг пишет ../stress-logs относительно cwd (cli/), т.е. в root.
    const rcFiles = readdirSync(join(root, "stress-logs")).filter((f) => f.endsWith(".rc"))
    return { exit: proc.exitCode, rcFiles }
  }

  const FAILING_CARGO = [
    "#!/usr/bin/env bash",
    'echo "test flaky_thing ... FAILED"',
    "echo \"thread 'flaky_thing' panicked at x.rs:1:1:\"",
    "echo boom",
    "exit 101",
  ].join("\n")

  test("цикл переживает все упавшие прогоны: rc записан для каждого, шаг красный", () => {
    const body = stepBody("cargo test ×N")
    const { exit, rcFiles } = runStepBody(body, FAILING_CARGO)
    expect(rcFiles.length).toBe(3) // High раунда 1: до фикса rc-файл не появлялся никогда
    expect(exit).not.toBe(0) // джоба обязана быть красной
  }, 20000)

  test("красно-зелёная фиксура: голый pipeline без лова валит этот пин", () => {
    // Дословный дофиксный вариант (ревью раунд 1): pipeline под bash -e
    // убивает шаг на первом прогоне, rc-файлы не пишутся.
    const legacy = `set -o pipefail
mkdir -p ../stress-logs
overall=0
for i in $(seq 1 "$STRESS_RUNS"); do
  cargo test 2>&1 | tee "../stress-logs/run-$i.log"
  echo "$rc" > "../stress-logs/run-$i.rc"
done
exit "$overall"`
    const { exit, rcFiles } = runStepBody(legacy, FAILING_CARGO)
    expect(rcFiles.length).toBe(0)
    expect(exit).not.toBe(0)
  }, 20000)
})
