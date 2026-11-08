// tests/factory-review-stall.test.ts — врезка гварда ревью-тишины в тик
// фабрики (баг «вердикт и статус не применяются при успешной сессии
// ревьювера», задача 20261107142321): ход ревьювера завершился (тишина
// > порога), статус из ревью не ушёл → сигнал юзеру + Needs attention,
// раз за эпизод; свежая активность — гвард молчит; активность вернулась
// при стоявшем флаге — флаг снят; мёртвая ревью-сессия — штатный спавн.
// Фабрика гоняется в СПАВН-РЕБЁНКЕ с HOME от старта процесса: os.homedir()
// в bun игнорирует runtime-подмену env (инцидент 06.11).
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "factory-review-stall-child.ts")

interface TickCalls {
  comments: string[]
  itemUpdates: string[]
  prompts: number
  sessionCreates: number
}

interface StallVerdict {
  scenario: string
  tick1: TickCalls
  tick2: TickCalls
  reviewStallSignaled: boolean | null
  lastStatus: string
  reviewSessionID: string
}

describe("tickProject: гвард ревью-тишины (недоставленный вердикт ревьювера)", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-review-stall-"))
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

  function seatState(scenario: string): void {
    const reviewSessionID = scenario === "dead" ? "ses_dead" : "ses_rev1"
    const state = {
      tasks: {
        "20261107000000-row1": {
          sessionID: "", worktree: "", branch: "",
          bodyDoc: "20261107000000-doc1", title: "Зависшее ревью", project: "t",
          lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
          reviewSessionID, reviewAttempts: 0,
          lastStatus: "Ревью результата",
          ...(scenario === "recovered" ? { reviewStallSignaled: true } : {}),
        },
      },
    }
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), JSON.stringify(state))
  }

  async function runTick(scenario: "stall" | "healthy" | "recovered" | "dead"): Promise<StallVerdict> {
    seatState(scenario)
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token", REVIEW_STALL_SCENARIO: scenario,
        // явная песочница state: дефолт-изоляция ветки (задача 20261106082216)
        // не должна уводить сидированный state в branches/<ветка>
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
        // порог тишины фиксируем в дефолте (30 мин): сценарий stall сидирует
        // активность 31-мин давности — экспортированный на машине прогонов
        // UWUWU_FACTORY_STALL_MIN ≠ 30 не должен менять вердикт теста
        UWUWU_FACTORY_STALL_MIN: "",
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child упал (${code}): ${err.slice(0, 400)}`)
    const line = out.trim().split("\n").find((l) => l.startsWith("{"))
    if (line === undefined) throw new Error(`child не отчитался: ${out.slice(0, 200)} / ${err.slice(0, 200)}`)
    return JSON.parse(line) as StallVerdict
  }

  test("stall: ход завершился без доставки → сигнал юзеру + Needs attention, раз за эпизод", async () => {
    const v = await runTick("stall")
    // тик 1: недоставка детектирована — статус и сигнал
    expect(v.tick1.itemUpdates).toContain("k-status=Needs attention")
    expect(v.tick1.comments).toHaveLength(1)
    // текст сигнала: ведёт юзера рабочим маршрутом — ответ комментом, затем
    // перестановка статуса стадии (коммент в Needs attention агенту НЕ
    // доставляется — доставка только в агентском статусе)
    expect(v.tick1.comments[0]).toContain("ответь комментом")
    expect(v.tick1.comments[0]).toContain("переставь статус стадии")
    expect(v.lastStatus).toBe("Needs attention")
    // флаг эпизода стоит — второй тик молчит (анти-спам)
    expect(v.reviewStallSignaled).toBe(true)
    expect(v.tick2.itemUpdates).toHaveLength(0)
    expect(v.tick2.comments).toHaveLength(0)
  })

  test("healthy: активность ревьювера свежая — гвард молчит", async () => {
    const v = await runTick("healthy")
    expect(v.tick1.itemUpdates).toHaveLength(0)
    expect(v.tick1.comments).toHaveLength(0)
    expect(v.tick2.itemUpdates).toHaveLength(0)
    expect(v.reviewStallSignaled).toBe(null)
  })

  test("recovered: активность вернулась при стоявшем флаге — флаг снят без сигнала", async () => {
    const v = await runTick("recovered")
    expect(v.tick1.itemUpdates).toHaveLength(0)
    expect(v.tick1.comments).toHaveLength(0)
    expect(v.reviewStallSignaled).toBe(null)
    expect(v.tick2.itemUpdates).toHaveLength(0)
    expect(v.tick2.comments).toHaveLength(0)
  })

  test("dead: мёртвая ревью-сессия — штатный спавн routeTask, гвард не вмешивается", async () => {
    const v = await runTick("dead")
    // spawn-reviewer: новая сессия + доревью-промпт; сигнала юзеру нет
    expect(v.tick1.sessionCreates).toBe(1)
    expect(v.tick1.prompts).toBe(1)
    expect(v.tick1.comments).toHaveLength(0)
    expect(v.reviewSessionID).toBe("ses_new")
  })
})
