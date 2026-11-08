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
  /** очередь сигналов дримеру: имя файла → содержимое (done-контракт) */
  queueSignals: Record<string, string>
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

  async function runTick(
    scenario: "hold" | "done-clean" | "resolve" | "claim-skip" | "claim-dead", stateJson: string,
  ): Promise<GuardVerdict> {
    // Arrange: state и service.json песочницы — до старта ребёнка
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), stateJson)
    const queueDir = join(home, "dreamer-queue")
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token", GUARD_SCENARIO: scenario,
        // явная песочница state: дефолт-изоляция ветки (задача 20261106082216)
        // не должна уводить сидированный state в branches/<ветка>
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
        // песочница очереди дримера: фабрика пишет сигналы сюда, не в прод
        UWUWU_DREAMER_QUEUE_DIR: queueDir,
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

  test("полный цикл эпизода: hold → allow снял флаг → повторный hold даёт второй бэкап и алерт снова", async () => {
    // Arrange: вход в эпизод — флаг и первый бэкап
    await runTick("hold", JSON.stringify({ tasks: {} }))
    const flag1 = savedState().holdSignaled
    expect(flag1).toBeDefined()
    expect(suspectBackups().length).toBe(1)

    // Act: сигнатура ушла (allow при стоящем флаге) — демон обязан снять флаг
    const resolved = await runTick("resolve", readFileSync(join(home, ".local/state/uwuwu-factory/state.json"), "utf-8"))

    // Assert: флаг стёрт, спавнов нет, эпизод завершён
    expect(resolved.holdSignaled).toBeNull()
    expect(resolved.sessionCreates).toBe(0)
    expect(savedState().holdSignaled).toBeUndefined()

    // Act: сигнатура вернулась — новый эпизод с alert=first
    const reentered = await runTick("hold", readFileSync(join(home, ".local/state/uwuwu-factory/state.json"), "utf-8"))

    // Assert: алерт доставлен снова, второй бэкап появился, флаг свежий
    expect(reentered.holdSignaled).not.toBeNull()
    expect(reentered.comments).toEqual(["20261106000000-doc1"])
    expect(reentered.taskCount).toBe(0)
    expect(suspectBackups().length).toBe(2)
    expect(savedState().holdSignaled === flag1).toBe(false)
  })

  test("done-clean: чистка трека при Done — сессионные ячейки очищены, done-сигнал дримеру (сессии НЕ удаляются)", async () => {
    // Arrange: трекнутая задача с сессией и историей корней; строка доски Done
    const state = {
      tasks: {
        "20261105000000-row1": {
          sessionID: "ses_old1", worktree: "", branch: "",
          bodyDoc: "20261105000000-doc1", title: "Закрытая задача",
          lastCommentAt: 0, lastCommentId: "",
          createdAt: 1, attempts: 0, project: "t", lastStatus: "In Progress",
          sessionHistory: ["ses_gen0", "ses_old1"],
        },
      },
    }

    // Act
    const verdict = await runTick("done-clean", JSON.stringify(state))

    // Assert: трек удалён, сессии НЕ удаляются из опенкода (решение владельца
    // 08.11: переоткрытие задачи должно находить все сессии)
    expect(verdict.taskCount).toBe(0)
    expect(verdict.sessionDeletes).toBe(0)
    expect(verdict.sessionCreates).toBe(0)
    // Assert: все сессионные ячейки очищены на строке (пустой текст)
    expect(verdict.itemUpdates).toEqual(["k-dev=", "k-rev=", "k-test="])
    // Assert: done-сигнал в очереди — полная история корней (включая легаси
    // текущий sessionID), мастер-чекаут проекта как directory
    const doneKey = Object.keys(verdict.queueSignals).find((k) => k.startsWith("done-"))
    expect(doneKey).toBe("done-20261105000000-row1.json")
    const done = JSON.parse(verdict.queueSignals[doneKey as string]) as {
      rowId: string; project: string; rootSessions: string[]; directory: string
    }
    expect(done.rowId).toBe("20261105000000-row1")
    expect(done.project).toBe("t")
    expect(done.rootSessions.sort()).toEqual(["ses_gen0", "ses_old1"])
    expect(done.directory).toBe("/tmp/x")
    // Assert: Hold-эпизода нет — легитимная чистка не алертит
    expect(verdict.holdSignaled).toBeNull()
    expect(verdict.comments).toEqual([])
  })

  test("claim-skip: живая дев-заявка без трека — не спавнится, HOLD-эпизода нет", async () => {
    // Act: пустой state + untracked In Progress с непустой «Сессия dev»
    // (мок сессии отвечает живостью)
    const verdict = await runTick("claim-skip", JSON.stringify({ tasks: {} }))

    // Assert: фабрика не заспавнила дубль — внешняя работа уже идёт
    expect(verdict.sessionCreates).toBe(0)
    expect(verdict.prompts).toBe(0)
    // Assert: живая заявка — не сирота: HOLD-эпизод не открыт, алерта нет
    expect(verdict.holdSignaled).toBeNull()
    expect(verdict.comments).toEqual([])
    expect(verdict.taskCount).toBe(0)
  })

  test("claim-dead: мёртвая дев-заявка без трека — сирота как прежде, спавнится при allow", async () => {
    // Act: пустой state + untracked In Progress с мёртвой ячейкой (мок 404);
    // сирот = 1 ≤ lossMax → allow
    const verdict = await runTick("claim-dead", JSON.stringify({ tasks: {} }))

    // Assert: ре-спавн компенсирует мёртвую заявку (без него — зомби-строка:
    // ни спавна, ни stall-сигнала, ни wakeup-роутинга)
    expect(verdict.sessionCreates).toBe(1)
    expect(verdict.prompts).toBe(1)
    // Assert: 1 сирота в пределах lossMax — HOLD-эпизода нет
    expect(verdict.holdSignaled).toBeNull()
    expect(verdict.comments).toEqual([])
    expect(verdict.taskCount).toBe(1)
  })
})
