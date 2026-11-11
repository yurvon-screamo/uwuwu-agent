// tests/cron-agent-drift.test.ts — дрейф агента вечной сессии: смена
// custom-agent карточки при живой сессии (эпизод 09.11 — тихий «Agent not
// found» без ретрая). Интеграционный прогон тика на mock-серверах
// (SiYuan + opencode + CLI), сценарий — дочерний процесс
// (cron-agent-drift-child.ts). Закрывает три POST-дороги вечного контура:
// штатный файр (recreate при idle-дрейфе, defer на живом ходе), регресс без
// смены агента, pendingRun-ретрай (recreate без дубль-рана).
import { existsSync, rmSync, mkdirSync, cpSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const CHILD = join(import.meta.dir, "cron-agent-drift-child.ts")
/** HOME-песочница чайлда: os.homedir() в Bun читает HOME, заданный НА СПАВНЕ —
 *  дефы тестовых агентов материализуются в песочнице, реальный конфиг не
 *  трогается. service.json копируется в песочницу: restCall демона читает
 *  реестр сервиса на каждый вызов, сам REST замокан во child. */
const CHILD_HOME = "/tmp/opencode/cron-agent-drift-home"
const SERVICE_REG = join(process.env.HOME ?? "", ".local/state/opencode/service.json")
const hasServiceReg = existsSync(SERVICE_REG)

interface ChildReport {
  error?: string
  stages: { label: string; state: Record<string, Record<string, unknown>>; prompts: number; creations: number; spawns: number; runCards: number; deletes: number }[]
  creations: { title: string; agent: unknown; model: unknown }[]
  promptCalls: { sessionID: string; text: string; delivery: unknown }[]
  deletes: string[]
  comments: { docId: string; content: string }[]
  strictPathPin?: boolean
  logLines: string[]
  masterDocId: string
  agents: string[]
  runCardIds: string[]
  strictPathPin?: boolean
}

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

const ROW = "20261109000000-master"
const [AGENT_A, AGENT_B, AGENT_C] = ["test-drift-a", "test-drift-b", "test-drift-c"]
const DEF_MODEL = { providerID: "zai-coding-plan", id: "test-model", variant: "max" }

describe.skipIf(!hasServiceReg)("tick × дрейф агента вечной сессии (смена custom-agent карточки)", () => {
  test("idle-дрейф → recreate+DELETE+лог; живой ход → defer; без смены → без recreate; ретрай при дрейфе → recreate без дубль-рана", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    expect(report.strictPathPin).toBe(true) // регресс-пин класса PR #207: мок отвергает двойной /api-префикс

    const [t1, t2, t3, t4, t5, t6, t7] = report.stages
    const master = (s: typeof t1) => s.state[ROW] as Record<string, unknown>
    expect(master(t1)).toBeDefined()

    // T1: штатный файр — одна вечная сессия с агентом A и моделью из дефа
    expect(t1.creations).toBe(1)
    expect(report.creations[0].agent).toBe(AGENT_A)
    expect(report.creations[0].model).toEqual(DEF_MODEL)
    expect(master(t1).sessionID).toBe("ses_mock_1")
    expect(master(t1).pendingRun).toBeUndefined()
    expect(t1.deletes).toBe(0)

    // T2: смена карточки A→B при idle-сессии → recreate (gone-путь):
    // вторая сессия под агентом B, старая DELETE (сироту не оставляем),
    // конверт приземлился в НОВУЮ сессию, честный лог
    expect(t2.creations).toBe(2)
    expect(report.creations[1].agent).toBe(AGENT_B)
    expect(report.creations[1].model).toEqual(DEF_MODEL)
    expect(t2.deletes).toBe(1) // identity удалённой — в финальном списке ниже
    expect(master(t2).sessionID).toBe("ses_mock_2")
    expect(master(t2).pendingRun).toBeUndefined()
    expect(report.promptCalls[1].sessionID).toBe("ses_mock_2")
    expect(report.promptCalls[1].delivery).toBeUndefined()
    expect(report.logLines.some((l) => l.includes(`агент сменён (${AGENT_A} → ${AGENT_B})`) && l.includes("контекст сброшен"))).toBe(true)

    // T3: файр без смены агента → без recreate (регресс), обычный prompt
    expect(t3.creations).toBe(2)
    expect(master(t3).sessionID).toBe("ses_mock_2")
    expect(report.promptCalls[2].sessionID).toBe("ses_mock_2")
    expect(report.promptCalls[2].delivery).toBeUndefined()
    expect(t3.deletes).toBe(1)

    // T4: смена B→C при живом ходе → defer: ход не рвём — стирт в ту же
    // сессию, recreate нет, лог о переносе на следующий файр
    expect(t4.creations).toBe(2)
    expect(report.promptCalls[3].sessionID).toBe("ses_mock_2")
    expect(report.promptCalls[3].delivery).toBe("steer")
    expect(report.logLines.some((l) => l.includes("на живом ходе"))).toBe(true)

    // T5: ход дошёл до idle, дрейф C → recreate под C, старая DELETE
    expect(t5.creations).toBe(3)
    expect(report.creations[2].agent).toBe(AGENT_C)
    expect(t5.deletes).toBe(2)
    expect(master(t5).sessionID).toBe("ses_mock_3")

    // T6: POST падает → pendingRun (ран не теряется), lastFiredAt не сжигается
    expect(t6.prompts).toBe(6)
    const pending = master(t6).pendingRun as { rowId: string; prompt: string; attemptAt: number } | undefined
    if (pending === undefined) throw new Error("mock: T6 не оставил pendingRun — сценарий сломан")
    expect(pending.rowId).toBe("20261109000000-run6")
    expect(master(t6).lastFiredAt).toBe(master(t5).lastFiredAt)
    expect(report.logLines.some((l) => l.includes("файр не приземлился — pendingRun, ретрай следующим тиком"))).toBe(true)

    // T7: ретрай при новом дрейфе (C→B) → recreate в новую сессию с
    // ИСХОДНЫМ конвертом: ран НЕ дублируется, pendingRun снят, state живёт
    expect(t7.creations).toBe(4)
    expect(report.creations[3].agent).toBe(AGENT_B)
    expect(master(t7).sessionID).toBe("ses_mock_4")
    expect(master(t7).pendingRun).toBeUndefined()
    expect(report.promptCalls[6].sessionID).toBe("ses_mock_4")
    expect(report.promptCalls[6].text).toBe(pending.prompt)
    expect(report.promptCalls[6].text).toContain("20261109000000-run6")
    expect(master(t7).lastFiredAt).toBeGreaterThan(master(t6).lastFiredAt as number)
    expect(report.deletes).toEqual(["ses_mock_1", "ses_mock_2", "ses_mock_3"])
    expect(report.runCardIds).toEqual([
      "20261109000000-run1", "20261109000000-run2", "20261109000000-run3",
      "20261109000000-run4", "20261109000000-run5", "20261109000000-run6",
      "20261109000000-run7", // T8: ран создан до failed POST, ретраи не дублируют
    ])
    expect(report.logLines.some((l) => l.includes(`агент сменён (${AGENT_C} → ${AGENT_B})`))).toBe(true)

    // — ветка гейта дефа при дрейфе в resolvePendingRun (допокрытие tester,
    // заявленное поведение: «дефа нет → refuseOnce, без создания мёртвой сессии») —

    // T8: новый failed POST → pendingRun run7, creations/deletes не растут
    const [t8, t9, t10] = report.stages.slice(7) as [typeof t1, typeof t1, typeof t1]
    expect(master(t8).pendingRun).toBeDefined()
    expect((master(t8).pendingRun as { rowId: string }).rowId).toBe("20261109000000-run7")
    expect(t8.creations).toBe(4)
    expect(t8.deletes).toBe(3)

    // штатный дрейф-сценарий (T1–T7) ни разу не комментил отказ
    expect(report.comments.length).toBe(1)

    // T9: ретрай при дрейфе B→агент-без-дефа → refuseOnce, recreate НЕТ:
    // мёртвой сессии не создано, слот не сжигается (pendingRun жив),
    // коммент в мастер с причиной, дедуп-метка в state
    expect(t9.creations).toBe(4) // recreate не случился
    expect(t9.deletes).toBe(3)
    expect(master(t9).pendingRun).toBeDefined() // слот не сожжён — ретрай возможен
    expect((master(t9).pendingRun as { rowId: string }).rowId).toBe("20261109000000-run7")
    expect(master(t9).lastRefusal).toContain("test-drift-missing")
    expect(master(t9).lastRefusal).toContain("не найден")
    expect(report.comments.length).toBe(1)
    expect(report.comments[0].docId).toBe(report.masterDocId)
    expect(report.comments[0].content).toContain("крон-мастер не запускается:")
    expect(report.comments[0].content).toContain("test-drift-missing")
    expect(t9.prompts).toBe(8) // 7 до T8 + упавший POST T8: конверт в мёртвую цель НЕ улетел
    expect(t10.prompts).toBe(9) // T10: ретрай приземлил run7

    // T10: агент с дефом вернули → ретрай приземлил run7 в ту же сессию
    // (дрейфа нет), pendingRun снят, дедуп refusal сброшен (повторный отказ
    // комментится заново)
    expect(t10.creations).toBe(4) // recreate по-прежнему нет — агент совпадает
    expect(report.promptCalls[7].sessionID).toBe("ses_mock_4")
    expect(report.promptCalls[7].text).toContain("20261109000000-run7")
    expect(master(t10).pendingRun).toBeUndefined()
    expect(master(t10).lastRefusal).toBeUndefined()
    expect(master(t10).lastFiredAt).toBeGreaterThan(master(t8).lastFiredAt as number)
  })
})
