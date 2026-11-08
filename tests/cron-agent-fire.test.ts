// tests/cron-agent-fire.test.ts — боевой (не-DRY_RUN) файр вечного мастера,
// интеграционный прогон тика на mock-серверах (SiYuan + opencode + CLI).
// Закрывает главный путь вечного контура, который DRY_RUN-смоук не доигрывает:
// создание сессии (agent + модель из дефа) → ран-карточка → POST → state,
// стирт занятой сессии, pendingRun при неподтверждённом POST и ретрай с
// дедупом приземления без дубль-рана и дубль-промпта. Сценарий — дочерний
// процесс (cron-agent-fire-child.ts): env демона печётся на импорте.
import { mkdirSync, cpSync, rmSync, existsSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CHILD = join(import.meta.dir, "cron-agent-fire-child.ts")
/** HOME-песочница чайлда: os.homedir() в Bun читает HOME, заданный НА СПАВНЕ —
 *  деф тестового агента материализуется в песочнице, реальный конфиг не трогается.
 *  service.json копируется в песочницу: restCall демона читает реестр сервиса
 *  на каждый вызов, сам REST замокан во child. */
const CHILD_HOME = "/tmp/opencode/cron-agent-fire-home"
const SERVICE_REG = join(process.env.HOME ?? "", ".local/state/opencode/service.json")

interface ChildReport {
  error?: string
  stages: { label: string; state: Record<string, Record<string, unknown>>; prompts: number; creations: number; spawns: number; runCards: number }[]
  creations: { title: string; agent: unknown; model: unknown }[]
  promptCalls: { sessionID: string; text: string; delivery: unknown }[]
  spawnCalls: { argv: string[]; stdoutText: string }[]
  logLines: string[]
  agentsDir: string
  agent: string
  runCardIds: string[]
}

async function runChild(): Promise<ChildReport> {
  if (!hasServiceReg) throw new Error("скип-гард должен был отсечь прогон без реестра")
  rmSync(CHILD_HOME, { recursive: true, force: true })
  mkdirSync(join(CHILD_HOME, ".local/state/opencode"), { recursive: true })
  cpSync(SERVICE_REG, join(CHILD_HOME, ".local/state/opencode/service.json"))
  const proc = Bun.spawn(["bun", CHILD], {
    env: { ...process.env, HOME: CHILD_HOME },
    stdout: "pipe",
    stderr: "pipe",
  })
  const out = await new Response(proc.stdout).text()
  const errOut = await new Response(proc.stderr).text()
  await proc.exited
  const line = out.trim().split("\n").filter((l) => l.startsWith("{")).pop() ?? ""
  if (line === "") throw new Error(`child без JSON-отчёта; stderr: ${errOut.slice(0, 400)}`)
  return JSON.parse(line) as ChildReport
}

const ROW = "20261109000000-master"

/** Интеграционный риг с mock-границами, но живой реестр opencode-сервиса
 *  нужен демону (restCall читает его на каждый вызов) — без него скип
 *  (паттерн само-гарда интеграционных тестов, CI реестра не имеет). */
const hasServiceReg = existsSync(SERVICE_REG)

describe.skipIf(!hasServiceReg)("tick × боевой файр вечного мастера (custom-agent)", () => {
  test("gone→prompt создаёт сессию и ран; running→steer; failed POST→pendingRun; ретрай без due закрывает приземлением", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    expect(report.agentsDir.startsWith(`${CHILD_HOME}/`)).toBe(true)

    const [t1, t2, t3, t4] = report.stages
    const master = (s: typeof t1) => s.state[ROW] as Record<string, unknown>
    expect(master(t1)).toBeDefined()

    // T1: gone → одна вечная сессия с агентом и моделью из дефа, ран-карточка, POST ok
    expect(report.creations).toHaveLength(1)
    expect(report.creations[0].agent).toBe(report.agent)
    expect(report.creations[0].model).toEqual({ providerID: "zai-coding-plan", id: "test-model", variant: "max" })
    expect(t1.prompts).toBe(1)
    expect(report.promptCalls).toHaveLength(3)
    expect(report.promptCalls[0].sessionID).toBe("ses_mock_1")
    expect(report.promptCalls[0].delivery).toBeUndefined()
    expect(master(t1).sessionID).toBe("ses_mock_1")
    expect(master(t1).pendingRun).toBeUndefined()
    expect(report.runCardIds[0]).toBe("20261109000000-run1")
    expect(report.logLines.some((l) => l.includes("ран 20261109000000-run1 → вечная сессия ses_mock_1 (recreate, приземлился"))).toBe(true)

    // T2: сессия running → стирт в ТУ ЖЕ сессию, без нового создания
    expect(t2.creations).toBe(1)
    expect(t2.prompts).toBe(2)
    expect(report.promptCalls[1].delivery).toBe("steer")
    expect(master(t2).sessionID).toBe("ses_mock_1")
    expect(master(t2).lastFiredAt).toBeGreaterThan(master(t1).lastFiredAt as number)
    expect(report.logLines.some((l) => l.includes("(steer, приземлился"))).toBe(true)

    // T3: POST падает → ран не теряется: pendingRun {rowId, prompt, attemptAt},
    // lastFiredAt не сжигается (ретрай не должен ждать слота)
    expect(t3.prompts).toBe(3)
    const pending = master(t3).pendingRun as { rowId: string; prompt: string; attemptAt: number } | undefined
    if (pending === undefined) throw new Error("mock: T3 не оставил pendingRun — сценарий сломан")
    // каждый файр слота — своя ран-карточка (и стирт тоже); ран T3 в полёте
    expect(pending.rowId).toBe("20261109000000-run3")
    expect(pending.prompt.length).toBeGreaterThan(0)
    expect(typeof pending.attemptAt).toBe("number")
    expect(master(t3).lastFiredAt).toBe(master(t2).lastFiredAt)
    expect(report.logLines.some((l) => l.includes("файр не приземлился — pendingRun, ретрай следующим тиком"))).toBe(true)

    // T4: ретрай до due-слота: промпт приземлился ранее → без повторного POST,
    // без новой сессии и без дубль-рана (reconcile клеток не в счёт),
    // pendingRun снят, lastFiredAt = приземление
    expect(t4.prompts).toBe(3)
    expect(t4.creations).toBe(1)
    expect(t4.runCards).toBe(t3.runCards)
    expect(master(t4).pendingRun).toBeUndefined()
    expect(master(t4).lastFiredAt).toBe(pending.attemptAt + 500)
    expect(report.runCardIds).toEqual(["20261109000000-run1", "20261109000000-run2", "20261109000000-run3"])
    expect(report.logLines.some((l) => l.includes("промпт файра приземлился ранее") && l.includes("ретрай не нужен"))).toBe(true)
  })
})
