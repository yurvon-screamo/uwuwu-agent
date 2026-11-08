// tests/cron-agent-dryrun.test.ts — DRY_RUN тика вечного мастера (custom-agent),
// два сценария одной доской: деф агента отсутствует → отказ без записей
// (коммент/state.json/CLI-спавн гардятся); деф на месте → позитивная ветка
// решений в логе, ноль opencode-REST (файр до POST не доходит) и ноль мутаций.
// Сценарий — дочерний процесс (cron-agent-child.ts): DRY_RUN печётся на импорте
// демона (паттерн cron-dryrun-child.ts).
import { describe, expect, test } from "bun:test"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "cron-agent-child.ts")

interface ChildReport {
  error?: string
  state?: Record<string, { lastRefusal?: string; pendingRun?: unknown }>
  calls: { kind: string; name: string }[]
  logLines: string[]
  agentOk: string
  agentMissing: string
}

async function runChild(): Promise<ChildReport> {
  const proc = Bun.spawn(["bun", CHILD], {
    env: { ...process.env },
    stdout: "pipe",
    stderr: "pipe",
  })
  const out = await new Response(proc.stdout).text()
  await proc.exited
  const line = out.trim().split("\n").filter((l) => l.startsWith("{")).pop() ?? ""
  return JSON.parse(line) as ChildReport
}

describe("tick × DRY_RUN × вечный мастер (custom-agent)", () => {
  test("деф без missing — отказ без записей; деф на месте — решения в логе, файр не доходит до POST", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    // ни одного CLI-спавна (ран-карточка), ни одной записи в доки (коммент)
    expect(report.calls.filter((c) => c.kind === "spawn")).toEqual([])
    expect(report.calls.filter((c) => c.name.includes("appendBlock"))).toEqual([])
    // файр не доходит до opencode REST: ни статуса сессии, ни POST промпта
    expect(report.calls.filter((c) => c.name.includes("/session"))).toEqual([])
    // отказ по missing-агенту зафиксирован in-memory (дедуп повторов)
    const missing = Object.entries(report.state ?? {}).find(([k]) => k === "20261109000000-rowmiss")
    expect(missing?.[1]?.lastRefusal ?? "").toContain("не найден")
    // позитивная ветка: решения по вечному мастеру в логе, файр не создан
    expect(report.logLines.some((l) => l.includes("вечный мастер «мастер-ok»") && l.includes("ран-карточка и файр не созданы"))).toBe(true)
    expect(report.logLines.some((l) => l.includes(`agent «${report.agentOk}»`) && l.includes("#max"))).toBe(true)
    expect(report.logLines.some((l) => l.includes(`«${report.agentMissing}» не найден`))).toBe(true)
  })
})
