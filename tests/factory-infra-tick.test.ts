// tests/factory-infra-tick.test.ts — врезка самоисцеления трека в тик
// фабрики (баг «bodyDoc пуст у всех треков», задача 20261105214942):
// state с пустым bodyDoc + привязанная строка доски → adopt до выборки
// комментов, засев bodyUpdated-базлайна, идемпотентность на повторном тике.
// Фабрика гоняется в СПАВН-РЕБЁНКЕ с HOME от старта процесса: os.homedir()
// в bun игнорирует runtime-подмену env (инцидент 06.11: тест с runtime-HOME
// ходил в прод-state) — изоляция только через новый процесс.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "factory-tick-child.ts")
const ROW_ID = "20261105000000-row1"
const BODY_DOC = "20261105000000-bodydoc"

describe("tickProject: самоисцеление пустого bodyDoc трека", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-tick-"))
    mkdirSync(join(home, ".local/state/uwuwu-factory"), { recursive: true })
    mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
    writeFileSync(
      join(home, ".local/state/opencode/service.json"),
      JSON.stringify({ url: "http://127.0.0.1:9", password: "pw" }),
    )
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), JSON.stringify({
      tasks: {
        [ROW_ID]: {
          sessionID: "ses_livetest0000000000000000000", worktree: "", branch: "",
          bodyDoc: "", title: "Тестовая задача",
          lastCommentAt: 0, lastCommentId: "",
          createdAt: 1, attempts: 0, project: "t", lastStatus: "In Progress",
        },
      },
    }))
  })

  afterEach(() => {
    rmSync(home, { recursive: true, force: true })
  })

  /** Ребёнок с HOME от старта процесса: state/лог/REST — только песочница. */
  async function runTick(): Promise<{ bodyDoc: string | null; bodyUpdated: string | null; exportIds: string[]; prompts: number }> {
    const proc = Bun.spawn(["bun", CHILD], {
      env: { ...process.env, HOME: home, UWUWU_SIYUAN_TOKEN: "test-token" },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${code}): ${(err.trim() || out.trim()).slice(0, 300)}`)
    return JSON.parse(out.trim()) as { bodyDoc: string | null; bodyUpdated: string | null; exportIds: string[]; prompts: number }
  }

  const savedState = (): { tasks: Record<string, { bodyDoc: string; bodyUpdated?: string }> } =>
    JSON.parse(readFileSync(join(home, ".local/state/uwuwu-factory/state.json"), "utf-8"))

  test("тик-1: adopt до выборки комментов + засев bodyUpdated", async () => {
    // Act
    const verdict = await runTick()

    // Assert: state — тело принято, базлайн засеян
    const state = savedState()
    expect(state.tasks[ROW_ID].bodyDoc).toBe(BODY_DOC)
    expect(state.tasks[ROW_ID].bodyUpdated).toBe("20261105120000")
    // Assert: комменты прочитаны УЖЕ по исцелённому телу (пустой bodyDoc
    // дал бы ранний return без вызова exportMdContent)
    expect(verdict.exportIds).toEqual([BODY_DOC])
    // Assert: будильников нет (свежих комментов не было)
    expect(verdict.prompts).toBe(0)
  })

  test("тик-2: adopt не повторяется (идемпотентность)", async () => {
    // Arrange: тик-1 уже исцелил трек
    await runTick()
    const afterTick1 = savedState()

    // Act
    const verdict = await runTick()

    // Assert: тело и базлайн не перезаписаны заново, трек жив
    const afterTick2 = savedState()
    expect(afterTick2.tasks[ROW_ID].bodyDoc).toBe(BODY_DOC)
    expect(afterTick2.tasks[ROW_ID].bodyUpdated).toBe("20261105120000")
    expect(afterTick2).toEqual(afterTick1)
    // комменты по-прежнему читаются по исцелённому телу
    expect(verdict.exportIds).toEqual([BODY_DOC])
    expect(verdict.prompts).toBe(0)
  })
})
