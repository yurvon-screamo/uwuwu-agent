// tests/cron-compact.test.ts — автокомпакт вечной сессии в тике крона,
// интеграционный прогон на mock-серверах (SiYuan + opencode + CLI), паттерн
// cron-agent-fire. Закрывает позицию чека ДО due-гейта (ревью раунда 1,
// Common-2: после гейта чек доживал бы только до моментов файра — мисплейсмент
// прошёл бы юнит-тесты тихо). Сценарии: T1 idle+раздутая сессия → ровно один
// POST /compact и метка в state; T2 коулдаун глушит повтор; T3 pendingRun
// приоритетнее компакции. Риг с mock-границами, но живой реестр opencode-
// сервиса нужен демону (restCall читает его на каждый вызов) — без него скип
// (CI реестра не имеет).
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CHILD = join(import.meta.dir, "cron-compact-child.ts")
const CHILD_HOME = "/tmp/opencode/cron-compact-home"
const SERVICE_REG = join(process.env.HOME ?? "", ".local/state/opencode/service.json")

interface ChildReport {
  error?: string
  stages: { label: string; state: Record<string, Record<string, unknown>>; compacts: number }[]
  compactCalls: string[]
  spawnCalls: string[][]
  logLines: string[]
  compactEnv: { idleSec?: string }
}

const ROW = "20261110000000-master"

async function runChild(): Promise<ChildReport> {
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

const hasServiceReg = existsSync(SERVICE_REG)

describe.skipIf(!hasServiceReg)("tick × автокомпакт вечного мастера (до due-гейта)", () => {
  test("недуевый мастер: idle+раздутая → один POST; коулдаун глушит; pendingRun приоритетнее", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    expect(report.compactEnv.idleSec).toBe("240")

    const master = (s: (typeof report.stages)[number]) => s.state[ROW] as Record<string, unknown>
    const [t1, t2, t3] = report.stages
    expect(master(t1)).toBeDefined()

    // T1: мастер не due, сессия idle 5ч с контекстом 50% → ровно один POST
    // /compact, метка admission персистена ДО POST
    expect(t1.compacts).toBe(1)
    expect(report.compactCalls).toEqual(["ses_mock_1"])
    expect(master(t1).lastCompactAt).toBeGreaterThan(0)
    expect(master(t1).pendingRun).toBeUndefined()
    expect(report.logLines.some((l) => l.includes("заявка приземлена") && l.includes("ses_mock_1"))).toBe(true)
    // файр не случался: расписание 1d, ран-карточек нет — только reconcile клеток
    expect(report.spawnCalls.every((a) => a[2] === "edit")).toBe(true)

    // T2: метка admission свежая (коулдаун 1ч) → POST не повторяется
    expect(t2.compacts).toBe(1)
    expect(master(t2).lastCompactAt).toBe(master(t1).lastCompactAt)

    // T3: метка состарена рукой сценария, но pendingRun в полёте — чек
    // скипается (POST /compact не звался), файр-ретрай доигрывает: промпт
    // приземлился, pendingRun снят, lastFiredAt доехал до приземления.
    // Запись перезаписывается стартом нового хода БЕЗ lastCompactAt —
    // свежий ход сам ре-армит idle-границу, метка не нужна
    expect(t3.compacts).toBe(1)
    expect(master(t3).pendingRun).toBeUndefined()
    expect(master(t3).lastCompactAt).toBeUndefined()
    expect(master(t3).lastFiredAt).toBeGreaterThan(master(t1).lastFiredAt as number)
  })
})
