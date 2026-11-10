// tests/factory-testing-stage.test.ts — врезка стадии «Тестирование» в тик
// фабрики: карантинный вход untracked-строки (даунтайм демона на переходе) —
// карантинная запись + спавн тестировщика, дев-волна НЕ спавнится, повторный
// тик не плодит дубль-спавн; дельта-дотест в ту же сессию; fail-вердикт —
// возврат девелоперу с пережитой тест-сессией. Фабрика гоняется в
// СПАВН-РЕБЁНКЕ с HOME от старта процесса (инцидент 06.11).
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "factory-testing-stage-child.ts")
const ROW_ID = "20261108000000-row1"
const DOC_ID = "20261108000000-doc1"

interface TickCalls {
  sessionCreates: string[]
  prompts: string[]
  itemUpdates: string[]
  comments: string[]
}

interface Tick3Calls {
  sessionCreates: string[]
  itemUpdates: string[]
  comments: string[]
}

interface StageVerdict {
  scenario: string
  tick1: TickCalls
  tick2: TickCalls
  tick3: Tick3Calls
  sessionID: string | null
  testSessionID: string | null
  testAttempts: number | null
  reviewSessionID: string | null
  lastStatus: string | null
}

describe("tickProject: стадия «Тестирование»", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-testing-stage-"))
    mkdirSync(join(home, ".local/state/uwuwu-factory"), { recursive: true })
    mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
    writeFileSync(
      join(home, ".local/state/opencode/service.json"),
      JSON.stringify({ url: "http://127.0.0.1:9", password: "pw" }),
    )
  })

  afterEach(() => {
    rmSync(home, { recursive: true, force: true })
  })

  function seatState(scenario: "quarantine" | "nocell" | "rerun" | "bug" | "worktree"): void {
    // quarantine/nocell/worktree — untracked строка (карантинный вход,
    // даунтайм демона, живой агентский ворктри без трека); остальным
    // нужен живой трек с тест-сессией
    const tasks = scenario === "quarantine" || scenario === "nocell" || scenario === "worktree"
      ? {}
      : {
          [ROW_ID]: {
            // дев-сессия: в rerun девелопер уже отдал мяч (карантин), в bug —
            // жив (возврат промптом в ту же сессию)
            sessionID: scenario === "bug" ? "ses_dev1" : "",
            worktree: "", branch: "",
            bodyDoc: DOC_ID, title: "Тестовая стадия", project: "t",
            lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
            testSessionID: "ses_t1",
            testAttempts: 0,
            lastStatus: scenario === "bug" ? "Тестирование" : "Ревью результата",
          },
        }
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), JSON.stringify({ tasks }))
  }

  async function runTick(scenario: "quarantine" | "nocell" | "rerun" | "bug" | "worktree"): Promise<StageVerdict> {
    seatState(scenario)
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token", TESTING_STAGE_SCENARIO: scenario,
        // явная песочница state: дефолт-изоляция ветки (задача 20261106082216)
        // не должна уводить сидированный state в branches/<ветка>
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${scenario}) exited ${code}: ${err.slice(0, 400)}`)
    return JSON.parse(out) as StageVerdict
  }

  test("quarantine: untracked «Тестирование» → карантинная запись + спавн тестировщика, дев-волна молчит", async () => {
    const v = await runTick("quarantine")
    // спавн ровно один, агент tester, тайтл стадии «тестирование»
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.tick1.sessionCreates[0]).toContain("tester:")
    expect(v.tick1.sessionCreates[0]).toContain("тестирование")
    // методичка в промпте, не дев-спавн-промпт
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("стадия тестирования")
    // карантинная запись: дев-сессии нет, тест-сессия записана, счётчик с нуля
    expect(v.sessionID).toBe("")
    expect(v.testSessionID).toBe("ses_new")
    expect(v.testAttempts).toBe(0)
    expect(v.lastStatus).toBe("Тестирование")
    // ячейка «Сессия тест» записана (k-test)
    expect(v.tick1.itemUpdates).toEqual(["k-test=ses_new"])
    // доска с живыми сессионными колонками — флаг ADR-054 не поднимается
    // (коммент только при расхождении: skipped пуст)
    expect(v.tick1.comments).toHaveLength(0)
    // повторный тик: дубль-спавна нет, повторного промпта нет
    expect(v.tick2.sessionCreates).toHaveLength(0)
    expect(v.tick2.prompts).toHaveLength(0)
    expect(v.tick2.comments).toHaveLength(0)
  })

  test("nocell: доска без «Сессия тест» — ячейка не пишется, флаг-коммент один на доску", async () => {
    const v = await runTick("nocell")
    // спавн состоялся (карантинная строка), state-запись тест-сессии есть:
    // привязка потеряна только в доске, не в треке
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.testSessionID).toBe("ses_new")
    // ячейка не пишется: колонки нет — запись ушла в no-op
    expect(v.tick1.itemUpdates).toEqual([])
    // громкий флаг: конфиг-расхождение доски доставлено владельцу (ADR-054)
    expect(v.tick1.comments).toHaveLength(1)
    expect(v.tick1.comments[0]).toContain("[factory]")
    expect(v.tick1.comments[0]).toContain("Сессия тест")
    // тихий тик: трек жив — ни спавна, ни повтора флага
    expect(v.tick2.sessionCreates).toHaveLength(0)
    expect(v.tick2.comments).toHaveLength(0)
    // рестарт-спавн (трек потерян): спавн повторяется, коммент — нет (дедуп)
    expect(v.tick3.sessionCreates).toHaveLength(1)
    expect(v.tick3.itemUpdates).toEqual([])
    expect(v.tick3.comments).toHaveLength(0)
  })

  test("rerun: живая тест-сессия + возврат из ревью → дельта-промпт в ту же сессию, без спавна", async () => {
    const v = await runTick("rerun")
    // новую сессию не создаём — инвариант 1 задача = 1 тестировщик
    expect(v.tick1.sessionCreates).toHaveLength(0)
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("устранение своего баг-репорта")
    expect(v.testSessionID).toBe("ses_t1")
    // граница перехода доедена за тик: статус строки усвоен
    expect(v.lastStatus).toBe("Тестирование")
    // повторный тик: повторной доставки нет
    expect(v.tick2.prompts).toHaveLength(0)
    expect(v.tick2.sessionCreates).toHaveLength(0)
  })

  test("bug: fail тестировщика → возврат девелоперу промптом, тест-сессия пережита", async () => {
    const v = await runTick("bug")
    // спавнов нет — дев-сессия жива, будим промптом
    expect(v.tick1.sessionCreates).toHaveLength(0)
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("вернулась к разработке")
    expect(v.tick1.prompts[0]).toContain("баг-репорт тестировщика")
    // тест-сессия сохранена при возврате (инвариант 1 задача = 1 тестировщик)
    expect(v.testSessionID).toBe("ses_t1")
    expect(v.lastStatus).toBe("In Progress")
    expect(v.tick2.prompts).toHaveLength(0)
    expect(v.tick2.sessionCreates).toHaveLength(0)
  })

  test("worktree: env:false с живым агентским ворктри — cwd и промпт ведут в живой путь, не фоллбэк", async () => {
    // e2e фикса 20261109133634-6ctgzew: инциденты 08–09.11 — промпт тестировщика
    // заявлял «ворктри не найден» при живом ~/agents-env/<rowId>/repo.
    const v = await runTick("worktree")
    const liveDir = join(home, "agents-env", ROW_ID, "repo")
    // спавн один (tester), cwd сессии — живой агентский ворктри, не cfg.local
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.tick1.sessionCreates[0]).toContain("tester:")
    expect(v.tick1.sessionCreates[0]).toContain(liveDir)
    // промпт называет живой ворктри и НЕ советует diff-only фоллбэк
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain(`Ворктри задачи: ${liveDir}`)
    expect(v.tick1.prompts[0]).not.toContain("Ворктри задачи не найден")
    // карантинная механика не изменилась: тест-сессия записана, дубля нет
    expect(v.testSessionID).toBe("ses_new")
    expect(v.lastStatus).toBe("Тестирование")
    expect(v.tick2.sessionCreates).toHaveLength(0)
    expect(v.tick2.prompts).toHaveLength(0)
  })
})
