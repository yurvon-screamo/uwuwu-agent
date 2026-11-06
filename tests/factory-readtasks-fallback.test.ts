// tests/factory-readtasks-fallback.test.ts — фоллбек тела по тайтлу для
// detached-строк доски (задача 20261105214942, паритет CLI body_doc_by_title).
// Резолв 0/1/≥2, санитизация тайтла ('/' → U+2215), кэш get_doc доски
// (один вызов на N строк), агрегированный лог дубль-сирот.
// Фабрика гоняется в СПАВН-РЕБЁНКЕ с HOME от старта процесса: os.homedir()
// в bun игнорирует runtime-подмену env (инцидент 06.11) — изоляция
// log()/конфига только через новый процесс.
import { afterEach, describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "factory-readtasks-child.ts")

interface Verdict {
  bodies: Record<string, string>
  hpathCalls: string[]
  boardGetDocCalls: number
  misses: string[]
  log: string
}

describe("readTasks: фоллбек тела по тайтлу (detached-строки)", () => {
  let home: string

  const runChild = async (scenario?: string): Promise<Verdict> => {
    home = mkdtempSync(join(tmpdir(), "factory-readtasks-"))
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env,
        HOME: home,
        UWUWU_SIYUAN_TOKEN: "test-token",
        // явная песочница state: дефолт-изоляция ветки (задача 20261106082216)
        // не должна уводить лог демона из прод-пути, который читает ребёнок
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
        ...(scenario !== undefined ? { SCENARIO: scenario } : {}),
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${code}): ${(err.trim() || out.trim()).slice(0, 400)}`)
    return JSON.parse(out.trim()) as Verdict
  }

  test("резолв 0/1/≥2, санитизация, кэш доски, агрегированный лог сирот", async () => {
    // Act
    const v = await runChild()

    // Assert: mock полный — ни один вызов не мимо
    expect(v.misses).toEqual([])

    // bound-строка: привязка используется, фоллбек не зовётся
    expect(v.bodies["row-bound"]).toBe("20261106000000-boundbod")
    // 1 док по тайтлу → тело
    expect(v.bodies["row-single"]).toBe("20261106000000-singlebod")
    // 0 доков → без тела
    expect(v.bodies["row-none"]).toBe("")
    // ≥2 доков одного тайтла → пусто (не угадываем — паритет CLI)
    expect(v.bodies["row-orphan"]).toBe("")
    // тайтл с '/' уходит в hpath санитизированным (U+2215) и резолвится
    expect(v.bodies["row-slash"]).toBe("20261106000000-slashbod")
    expect(v.hpathCalls).toContain("/uwuwu/Задачи x/Deploy: up\u{2215}down gateway")

    // known-строка (трек знает bodyDoc из state): тело из known-карты,
    // фоллбек не тикает — ни hpath-вызова, ни мимо-мока
    expect(v.bodies["row-known"]).toBe("20261106000000-knownbod")
    expect(v.hpathCalls).not.toContain("/uwuwu/Задачи x/Known док")

    // кэш: get_doc доски ровно один на проход
    expect(v.boardGetDocCalls).toBe(1)
    // bound и known не ходят в фоллбек: 8 detached − 1 известная = 7 вызовов
    expect(v.hpathCalls).toHaveLength(7)

    // дубль-сироты: агрегированный лог одной строкой, обе строки в ней
    const orphanLines = v.log.split("\n").filter((l) => l.includes("дубль-сироты"))
    expect(orphanLines).toHaveLength(1)
    expect(orphanLines[0]).toContain("row-orph") // rowId в логе срезается до 8 символов
    expect(orphanLines[0]).toContain("20261106000000-orpha")
    expect(orphanLines[0]).toContain("20261106000000-orphb")
  })

  test("сбой get_doc доски мемоизируется: один логический вызов и один лог на проход", async () => {
    // Act
    const v = await runChild("fail-get-doc")

    // Assert
    expect(v.misses).toEqual([])
    // 4 fetch = ОДИН логический get_doc × 4 ретрая post() (константа платформы);
    // без мемоизации было бы 7 × 4 — каждая detached-строка долбит упавшее ядро
    expect(v.boardGetDocCalls).toBe(4)
    expect(v.hpathCalls).toEqual([])
    // деградация — ровно одна строка лога от резолвера, per-row «не удался» — ни одной
    const failedLines = v.log.split("\n").filter((l) => l.includes("отключён на проход"))
    expect(failedLines).toHaveLength(1)
    expect(failedLines[0]).toContain("get_doc доски не удался")
    expect(v.log.split("\n").filter((l) => l.includes(": фоллбек тела по тайтлу не удался"))).toHaveLength(0)
    // bound-строка сохраняет привязку, detached — без тела (строка не теряется)
    expect(v.bodies["row-bound"]).toBe("20261106000000-boundbod")
    expect(v.bodies["row-single"]).toBe("")
  })

  test("SQL-лаг доски (пустые box/hpath): одна строка лога, фоллбек молча отключён", async () => {
    // Act
    const v = await runChild("sql-lag")

    // Assert
    expect(v.misses).toEqual([])
    // get_doc доски один (успешный конверт — без ретраев), hpath не вызывается
    expect(v.boardGetDocCalls).toBe(1)
    expect(v.hpathCalls).toEqual([])
    const lagLines = v.log.split("\n").filter((l) => l.includes("SQL-лаг"))
    expect(lagLines).toHaveLength(1)
    // симметрично fail-get-doc: per-row строк нет — деградация логируется раз на проход
    expect(v.log.split("\n").filter((l) => l.includes(": фоллбек тела по тайтлу не удался"))).toHaveLength(0)
    // detached-строки без тела, bound — с привязкой
    expect(v.bodies["row-single"]).toBe("")
    expect(v.bodies["row-bound"]).toBe("20261106000000-boundbod")
  })

  afterEach(() => {
    if (home !== undefined) rmSync(home, { recursive: true, force: true })
  })
})
