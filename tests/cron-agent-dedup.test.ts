// tests/cron-agent-dedup.test.ts — инцидентный дедуп pendingRun (сигнатура рана),
// интеграционный прогон тика на mock-серверах (SiYuan + opencode + CLI).
// Постановка: ручная копия файр-конверта (стирт оператора, текст разошёлся)
// приземлилась в вечную сессию, у демона pendingRun в полёте → ретрай обязан
// признать приземление по rowId и НЕ дублировать инструкцию (ADR-075). Пин
// чувствителен к обоим регрессам call-site: откат на текстовый дедуп
// (promptLandedAt — копии с разошедшимся текстом не узнаёт) и откат границы
// на attemptAt (копия до штампа pendingRun выпадает из окна) — оба дают
// дубль-POST. Сценарий — дочерний процесс (cron-agent-dedup-child.ts).
import { mkdirSync, cpSync, rmSync, existsSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CHILD = join(import.meta.dir, "cron-agent-dedup-child.ts")
/** HOME-песочница чайлда: restCall демона читает реестр сервиса на каждый
 *  вызов — копия реестра в песочнице, сам REST замокан во child. */
const CHILD_HOME = "/tmp/opencode/cron-agent-dedup-home"
const SERVICE_REG = join(process.env.HOME ?? "", ".local/state/opencode/service.json")

interface ChildReport {
  error?: string
  promptCalls: { sessionID: string; text: string; delivery: unknown }[]
  creations: Record<string, unknown>[]
  createCalls: string[][]
  masterA: { pendingRun: unknown; lastFiredAt: number }
  masterB: { pendingRun: unknown; lastFiredAt: number }
  logLines: string[]
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

/** Интеграционный риг с mock-границами; живой реестр opencode-сервиса нужен
 *  демону (restCall) — без него скип (паттерн cron-agent-fire.test.ts). */
const hasServiceReg = existsSync(SERVICE_REG)

describe.skipIf(!hasServiceReg)("tick × инцидентный дедуп pendingRun (сигнатура рана)", () => {
  test("ручная копия конверта с разошедшимся текстом признаётся — ретрай без дубль-POST и дубль-рана", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")

    // главный контракт: ни одного POST/создания сессии/дубль-ран-карточки —
    // дубль-инструкция в вечный контекст не доезжает (ADR-075)
    expect(report.promptCalls).toHaveLength(0)
    expect(report.creations).toHaveLength(0)
    expect(report.createCalls).toHaveLength(0)

    // мастер A (инцидент): копия приземлилась после неудачной попытки,
    // перед ней assistant-эхо — приземление по user-копии, не по эху
    expect(report.masterA.pendingRun).toBeNull()
    expect(report.masterA.lastFiredAt).toBe(ATTEMPT_AT + 500)

    // мастер B (окно low-1): копия приземлилась ДО штампа pendingRun
    // (attemptAt−5000 > lastFiredAt) в /inbox — старая граница attemptAt−2000
    // её теряла бы
    expect(report.masterB.pendingRun).toBeNull()
    expect(report.masterB.lastFiredAt).toBe(ATTEMPT_AT - 5000)

    // лог: оба рана признаны приземлившимися, ретрай не понадобился
    expect(report.logLines.filter((l) => l.includes("промпт файра приземлился ранее") && l.includes("ретрай не нужен"))).toHaveLength(2)
    expect(report.logLines.some((l) => l.includes("20261109103000-rowincA"))).toBe(true)
    expect(report.logLines.some((l) => l.includes("20261109103000-rowwinB"))).toBe(true)
  })
})

// времена рига — та же UTC-база, что во child (конвенция через границу
// процессов): штамп неудачной попытки (base + 10м); приземления копий —
// ±секунды от него
const ATTEMPT_AT = Date.UTC(2026, 10, 9, 10, 10, 0)
