// tests/factory-guard-tick.test.ts — врезка гарда каскада в тик фабрики
// (баг «пустой state.json — массовый re-spawn», задача 20261106032304):
// пустой state + доска с сиротами-спавнами → HOLD без спавнов (алерт + бэкап
// + флаг эпизода); трекнутая Done-строка → чистка state + очистка сессионных
// ячеек (инвариант гарда против ложных сирот).
// Фабрика гоняется в СПАВН-РЕБЁНКЕ с HOME от старта процесса: os.homedir()
// в bun игнорирует runtime-подмену env (инцидент 06.11) — изоляция только
// через новый процесс.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "factory-guard-child.ts")

interface GuardVerdict {
  scenario: string
  taskCount: number
  holdSignaled: string | null
  sessionCreates: number
  sessionDeletes: number
  prompts: number
  comments: string[]
  itemUpdates: string[]
}

describe("tickProject: гард каскада при потере state", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-guard-"))
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

  async function runTick(scenario: "hold" | "done-clean", stateJson: string): Promise<GuardVerdict> {
    // Arrange: state и service.json песочницы — до старта ребёнка
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), stateJson)
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token", GUARD_SCENARIO: scenario,
        // фиксируем дефолтные пороги: тест не зависит от env окружения
        UWUWU_FACTORY_SPAWN_LOSS_MAX: "", UWUWU_FACTORY_SPAWN_BURST_MAX: "",
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${code}): ${(err.trim() || out.trim()).slice(0, 300)}`)
    return JSON.parse(out.trim()) as GuardVerdict
  }

  const savedState = (): { tasks: Record<string, unknown>; holdSignaled?: string } =>
    JSON.parse(readFileSync(join(home, ".local/state/uwuwu-factory/state.json"), "utf-8"))

  const suspectBackups = (): string[] =>
    existsSync(join(home, ".local/state/uwuwu-factory"))
      ? readdirSync(join(home, ".local/state/uwuwu-factory")).filter((f) => f.startsWith("state.json.bak-suspect-"))
      : []

  test("инцидент-сценарий: пустой state, на доске сироты-спавны — HOLD, ноль спавнов, алерт и бэкап", async () => {
    // Act
    const verdict = await runTick("hold", JSON.stringify({ tasks: {} }))

    // Assert: спавнов нет вовсе (сессии не создаются, промпты не уходят)
    expect(verdict.sessionCreates).toBe(0)
    expect(verdict.prompts).toBe(0)
    // Assert: трекинг не пересобран спавном, эпизод помечен
    expect(verdict.taskCount).toBe(0)
    expect(verdict.holdSignaled).not.toBeNull()
    expect(Number.isNaN(Date.parse(verdict.holdSignaled ?? ""))).toBe(false)
    // Assert: алерт в тело первой затронутой задачи + датированный бэкап state
    expect(verdict.comments).toEqual(["20261106000000-doc1"])
    expect(suspectBackups().length).toBe(1)
    expect(savedState().tasks).toEqual({})
  })

  test("тик-2 в эпизоде: повторного алерта и второго бэкапа нет (анти-спам)", async () => {
    // Arrange: первый тик вошёл в эпизод
    await runTick("hold", JSON.stringify({ tasks: {} }))
    const afterTick1 = savedState()
    expect(afterTick1.holdSignaled).toBeDefined()

    // Act
    const verdict = await runTick("hold", readFileSync(join(home, ".local/state/uwuwu-factory/state.json"), "utf-8"))

    // Assert: HOLD держится, но алерт молчит (окно повтора не истекло)
    expect(verdict.taskCount).toBe(0)
    expect(verdict.holdSignaled).toBe(afterTick1.holdSignaled ?? null)
    expect(verdict.comments).toEqual([])
    expect(suspectBackups().length).toBe(1)
  })

  test("done-clean: чистка трека при Done — сессионные ячейки строки очищены (инвариант гарда)", async () => {
    // Arrange: трекнутая задача с сессией; строка доски Done с ячейкой «Сессия dev»
    const state = {
      tasks: {
        "20261105000000-row1": {
          sessionID: "ses_old1", worktree: "", branch: "",
          bodyDoc: "20261105000000-doc1", title: "Закрытая задача",
          lastCommentAt: 0, lastCommentId: "",
          createdAt: 1, attempts: 0, project: "t", lastStatus: "In Progress",
        },
      },
    }

    // Act
    const verdict = await runTick("done-clean", JSON.stringify(state))

    // Assert: трек удалён, сессия погашена, спавнов нет
    expect(verdict.taskCount).toBe(0)
    expect(verdict.sessionDeletes).toBe(1)
    expect(verdict.sessionCreates).toBe(0)
    // Assert: обе сессионные ячейки очищены на строке (пустой текст)
    expect(verdict.itemUpdates).toEqual(["k-dev=", "k-rev="])
    // Assert: Hold-эпизода нет — легитимная чистка не алертит
    expect(verdict.holdSignaled).toBeNull()
    expect(verdict.comments).toEqual([])
  })
})
