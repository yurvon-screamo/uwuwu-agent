// ci-stress-report.test.ts — поведенческие тесты scripts/ci/stress-report.sh
// (bun test, спавн скрипта на временных фикстурах). Парсер — единственный
// продукт стресс-джобы на красном пути: имена тестов + фрагменты паник.
// Контракт: markdown → stdout, ::error → stderr, отсутствие rc-файла =
// консервативно упавший прогон (шаг мог умереть до записи — таймаут/инфра).
// Живые доказательства: зелёный ран 42859796050, красный ран 42821058074
// (канарка, задача 20261110051831-qnkx3id).
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { afterAll, describe, expect, test } from "bun:test"

const SCRIPT = join(import.meta.dir, "..", "scripts", "ci", "stress-report.sh")

const tmpRoots: string[] = []

function makeLogsDir(): string {
  const dir = mkdtempSync(join(tmpdir(), "stress-report-test-"))
  tmpRoots.push(dir)
  return dir
}

function runScript(logsDir: string): { exit: number; stdout: string; stderr: string } {
  const proc = Bun.spawnSync(["bash", SCRIPT, logsDir])
  return {
    exit: proc.exitCode,
    stdout: new TextDecoder().decode(proc.stdout),
    stderr: new TextDecoder().decode(proc.stderr),
  }
}

function failRun(dir: string, n: number, body: string): void {
  writeFileSync(join(dir, `run-${n}.log`), body)
  writeFileSync(join(dir, `run-${n}.rc`), "101\n")
}

afterAll(() => {
  for (const dir of tmpRoots) rmSync(dir, { recursive: true, force: true })
})

describe("stress-report.sh — главный случай", () => {
  test("упавший прогон: имена тестов + паника в markdown, ::error в stderr", () => {
    const dir = makeLogsDir()
    failRun(
      dir,
      1,
      [
        "test calc_sum ... ok",
        "test flaky_thing ... FAILED",
        "",
        "thread 'flaky_thing' panicked at cli/src/search.rs:42:5:",
        "explicit panic: roundtrip exceeded budget",
        "test result: FAILED. 0 passed; 1 failed",
      ].join("\n"),
    )
    const { exit, stdout, stderr } = runScript(dir)
    expect(exit).toBe(0) // парсер не краснит джобу — она уже красная
    expect(stdout).toContain("## run-1: FAIL (rc=101)")
    expect(stdout).toContain("### Упавшие тесты")
    expect(stdout).toContain("flaky_thing")
    expect(stdout).toContain("panicked at cli/src/search.rs:42:5:")
    expect(stdout).toContain("roundtrip exceeded budget")
    // Потоки: аннотации не в summary (stdout), а в stderr — ревью раунд 1 Low.
    expect(stdout).not.toContain("::error::")
    expect(stderr).toContain("::error::run-1: упали: flaky_thing")
  })

  test("зелёный прогон не даёт сигнала", () => {
    const dir = makeLogsDir()
    writeFileSync(join(dir, "run-1.log"), "test foo ... ok\ntest result: ok. 2 passed\n")
    writeFileSync(join(dir, "run-1.rc"), "0\n")
    const { stdout, stderr } = runScript(dir)
    expect(stdout).not.toContain("FAIL")
    expect(stdout).not.toContain("::error::")
    expect(stderr).toContain("упавших прогонов не найдено")
  })

  test("смесь зелёных и упавших: сигнал только по упавшим", () => {
    const dir = makeLogsDir()
    writeFileSync(join(dir, "run-1.log"), "test foo ... ok\n")
    writeFileSync(join(dir, "run-1.rc"), "0\n")
    failRun(dir, 2, "test bar ... FAILED\nthread 'bar' panicked at x.rs:1:1:\nboom\n")
    const { stdout, stderr } = runScript(dir)
    expect(stdout).toContain("## run-2: FAIL (rc=101)")
    expect(stdout).not.toContain("run-1: FAIL")
    expect(stderr).toContain("run-2")
    expect(stderr).not.toContain("::error::run-1")
  })
})

describe("stress-report.sh — альтернативные ветки", () => {
  test("rc≠0 без FAILED-строк = сбой компиляции/инфраструктуры, отдельная аннотация", () => {
    const dir = makeLogsDir()
    failRun(dir, 1, "error[E0432]: unresolved import\n")
    const { stdout, stderr } = runScript(dir)
    expect(stdout).toContain("### Упавших тестов не найдено — сбой компиляции/инфраструктуры")
    expect(stderr).toContain("::error::run-1: сбой без упавших тестов (компиляция/инфраструктура)")
  })

  test("лог есть, rc-файла нет → консервативно упавший (rc=?)", () => {
    // Пин консервативной семантики: шаг цикла мог умереть до записи rc
    // (таймаут/инфра) — красная джоба обязана дать сигнал.
    const dir = makeLogsDir()
    writeFileSync(join(dir, "run-2.log"), "test flaky ... FAILED\nthread panicked at y.rs:9:1:\ndead\n")
    const { stdout, stderr } = runScript(dir)
    expect(stdout).toContain("## run-2: FAIL (rc=?)")
    expect(stdout).toContain("flaky")
    expect(stderr).toContain("::error::run-2: упали: flaky")
  })

  test("нет директории логов → громкий фейл, не тихий ноль", () => {
    const { exit, stderr } = runScript(join(makeLogsDir(), "absent"))
    expect(exit).not.toBe(0)
    expect(stderr).toContain("нет директории")
  })
})

describe("stress-report.sh — порядок и лимиты", () => {
  // Пер-тест таймаут выше дефолтного bun (5s): первый прогон файла под
  // холодным стартом bun+bash наблюдён убивающим спавн на пограничных 5s —
  // частичный stderr читался как ложный сигнал (найдено на этом тесте,
  // 11.11; скрипт на прогретом рантайме отрабатывает за сотни мс).
  test("сортировка прогонов числовая: run-10 после run-9, не после run-1", () => {
    const dir = makeLogsDir()
    for (let i = 1; i <= 12; i++) failRun(dir, i, `test t${i} ... FAILED\n`)
    const { exit, stdout } = runScript(dir)
    expect(exit).toBe(0)
    const ids = [...stdout.matchAll(/^## (run-\d+): FAIL/gm)].map((m) => m[1])
    expect(ids).toEqual([
      "run-1", "run-2", "run-3", "run-4", "run-5", "run-6",
      "run-7", "run-8", "run-9", "run-10", "run-11", "run-12",
    ])
  }, 20000)

  test("лимит error-аннотаций: не более 10 на прогон скрипта", () => {
    const dir = makeLogsDir()
    for (let i = 1; i <= 12; i++) failRun(dir, i, `test t${i} ... FAILED\n`)
    const { exit, stdout, stderr } = runScript(dir)
    expect(exit).toBe(0) // обрыв по таймауту рантайма ≠ продукт скрипта
    const annotations = stderr.split("::error::").length - 1
    expect(annotations).toBe(10) // лимит GitHub на шаг
    for (let i = 1; i <= 12; i++) expect(stdout).toContain(`run-${i}: FAIL`)
  }, 20000)
})
