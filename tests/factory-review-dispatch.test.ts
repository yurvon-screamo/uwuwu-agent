// tests/factory-review-dispatch.test.ts — фиксационный тест диспетчеризации
// ревьюверов по виду ревью (разделение агентов): «Ревью плана» спавнит
// plan-reviewer, «Ревью результата» — code-quality-reviewer; живая сессия
// другого вида (или legacy без вида) — спавн нового держателя, не дельта;
// живая сессия того же вида — дельта-промпт без спавна. Фабрика гоняется в
// СПАВН-РЕБЁНКЕ с HOME от старта процесса (инцидент 06.11).
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "factory-review-dispatch-child.ts")
const ROW_ID = "20261108000000-row1"

interface TickCalls {
  sessionCreates: string[]
  prompts: string[]
  itemUpdates: string[]
}

interface DispatchVerdict {
  scenario: string
  tick1: TickCalls
  devSessionID: string | null
  reviewSessionID: string | null
  reviewKind: string | null
  reviewAttempts: number | null
  lastStatus: string | null
}

type Scenario = "plan-spawn" | "plan-to-result" | "result-rerun" | "legacy-migrate" | "dev-restart-preserves"

describe("tickProject: диспетчеризация ревьюверов по виду ревью", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-review-dispatch-"))
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

  function seatState(scenario: Scenario): void {
    // живая ревью-сессия: у plan-to-result — план-сессия, у result-rerun и
    // dev-restart-preserves — result-сессия, у legacy-migrate — сессия старого
    // агента без вида
    const reviewSessionID = scenario === "plan-spawn" ? "" : "ses_rev1"
    const reviewKind = scenario === "plan-to-result"
      ? "plan"
      : scenario === "result-rerun" || scenario === "dev-restart-preserves"
      ? "result"
      : undefined
    const tasks = {
      [ROW_ID]: {
        sessionID: "",
        worktree: "", branch: "",
        bodyDoc: "20261108000000-doc1", title: "Диспетчер ревью", project: "t",
        lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
        reviewSessionID,
        reviewAttempts: scenario === "dev-restart-preserves" ? 2 : 0,
        ...(reviewKind !== undefined ? { reviewKind } : {}),
        lastStatus: scenario === "plan-spawn"
          ? "Design"
          : scenario === "dev-restart-preserves" ? "Ревью результата" : "In Progress",
      },
    }
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), JSON.stringify({ tasks }))
  }

  async function runTick(scenario: Scenario): Promise<DispatchVerdict> {
    seatState(scenario)
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token", REVIEW_DISPATCH_SCENARIO: scenario,
        // явная песочница state: дефолт-изоляция ветки не должна уводить
        // сидированный state в branches/<ветка>
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${scenario}) exited ${code}: ${err.slice(0, 400)}`)
    return JSON.parse(out) as DispatchVerdict
  }

  test("plan-spawn: «Ревью плана» без сессии → спавн plan-reviewer, state помнит вид", async () => {
    const v = await runTick("plan-spawn")
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.tick1.sessionCreates[0]).toContain("plan-reviewer:")
    // методичка адресована по имени агента
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("ревью плана")
    expect(v.tick1.prompts[0]).toContain("праймари-агент plan-reviewer")
    // state: сессия записана, вид запомнен, счётчик с нуля
    expect(v.reviewSessionID).toBe("ses_new")
    expect(v.reviewKind).toBe("plan")
    expect(v.reviewAttempts).toBe(0)
    // ячейка «Сессия ревью» перезаписана
    expect(v.tick1.itemUpdates).toEqual(["k-rev=ses_new"])
  })

  test("plan-to-result: живая план-сессия + «Ревью результата» → спавн нового держателя", async () => {
    const v = await runTick("plan-to-result")
    // вид в state не совпал — дельта план-сессии НЕ отправляется
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.tick1.sessionCreates[0]).toContain("code-quality-reviewer:")
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("праймари-агент code-quality-reviewer")
    // state: новый держатель и вид перезаписаны
    expect(v.reviewSessionID).toBe("ses_new")
    expect(v.reviewKind).toBe("result")
    // спавн нового держателя = свежая попытка
    expect(v.reviewAttempts).toBe(0)
  })

  test("result-rerun: живая result-сессия + возврат из фикса → дельта в ту же сессию", async () => {
    const v = await runTick("result-rerun")
    // инвариант 1 задача = 1 держатель вида: без спавна
    expect(v.tick1.sessionCreates).toHaveLength(0)
    expect(v.tick1.prompts).toHaveLength(1)
    expect(v.tick1.prompts[0]).toContain("Доревьюируй в этой же сессии")
    // state: держатель и вид не тронуты
    expect(v.reviewSessionID).toBe("ses_rev1")
    expect(v.reviewKind).toBe("result")
    // граница перехода доедена за тик
    expect(v.lastStatus).toBe("Ревью результата")
  })

  test("legacy-migrate: живая сессия без вида → переспавн нового держателя", async () => {
    const v = await runTick("legacy-migrate")
    // legacy-держатель (старый агент) не получает дельту — переспавн
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.tick1.sessionCreates[0]).toContain("code-quality-reviewer:")
    expect(v.reviewSessionID).toBe("ses_new")
    expect(v.reviewKind).toBe("result")
  })

  test("dev-restart-preserves: рестарт мёртвого девелопера сохраняет живую ревью-сессию и её вид", async () => {
    const v = await runTick("dev-restart-preserves")
    // девелопер переспавнен (спавн без праймари-агента — default)
    expect(v.tick1.sessionCreates).toHaveLength(1)
    expect(v.devSessionID).toBe("ses_new")
    // инвариант 1 задача = 1 держатель вида: ревью-трек пережил замену записи
    // state — сессия, счётчик попыток и вид перенесены preserveAgentSessions
    expect(v.reviewSessionID).toBe("ses_rev1")
    expect(v.reviewAttempts).toBe(2)
    expect(v.reviewKind).toBe("result")
  })
})
