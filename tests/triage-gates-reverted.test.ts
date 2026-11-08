// triage-gates-reverted.test.ts — регресс-пин реверса стоп-кранов триажа
// (задача 20261109005119, PR #185): гейткипинг при заведении задач снесён —
// Speed Bump Pushback (WIP-триггер + goal-сверка) из промпта агента, строка
// «сверка с целями» из канона AGENTS.md. Два слоя:
//  1) контент-пин: гейт-сигнатуры не вернулись в triage/agent.md и AGENTS.md
//     (частичный revert / конфликт мержа → тест падает и напоминает, что
//     возврат гейтов — осознанное решение, а не случайность);
//  2) интеграция деплой-цепочки: живой демон (bun triage/triage.ts, mock-TG,
//     песочный RUN_ROOT) материализует деф из репо в workspace — доезжает
//     именно новый промпт, без гейтов (syncAgentDef на старте, до ТГ-лупов).
// Вотчер черновиков у ребёнка выключен двойным гардом (UWUWU_TRIAGE_WATCH_SEC=0
// + UWUWU_SIYUAN_WS на пустой каталог → mcpConfigured() false) — прод-доски
// не читаются и не пишутся.
import { describe, test, expect, afterAll } from "bun:test"
import { createServer, type Server } from "node:http"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const REPO = join(import.meta.dir, "..")
const AGENT_MD = join(REPO, "triage", "agent.md")
const DEF_DST_REL = join(".local", "share", "uwuwu-triage", "workspace", ".opencode", "agents", "triage.md")

/** Гейт-сигнатуры до-реверсного промпта (origin/master до PR #185); «ерка с
 *  целя» ловит все падежи («Сверка/сверки/сверкой с целями»). */
const GATE_SIGNATURES = ["Speed Bump Pushback", "Триаж при заведении", "ерка с целя", "WIP-триггер"]

describe("реверс стоп-кранов триажа: гейт-контент не вернулся", () => {
  test("triage/agent.md — промпт агента без гейт-сигнатур", () => {
    const md = readFileSync(AGENT_MD, "utf-8")
    for (const s of GATE_SIGNATURES) {
      expect(md).not.toContain(s)
    }
  })

  test("AGENTS.md — канон-строка «Пустой статус» без goal-сверки", () => {
    const md = readFileSync(join(REPO, "AGENTS.md"), "utf-8")
    expect(md).not.toContain("ерка с целя")
    expect(md).not.toContain("WIP-триггер")
  })

  test("triage/README.md — описание флоу демона без goal list", () => {
    const md = readFileSync(join(REPO, "triage", "README.md"), "utf-8")
    expect(md).not.toContain("goal list")
  })
})

// ——— интеграция: живой демон материализует деф из репо ———

interface MockTg {
  server: Server
  port: number
}

/** Mock api.telegram.org: getMe — ок, getUpdates — долгий поллинг пустоты. */
function startMockTg(): Promise<MockTg> {
  return new Promise((resolve) => {
    const server = createServer((req, res) => {
      const method = (req.url ?? "").split("/").at(-1) ?? ""
      if (method === "getUpdates") {
        setTimeout(() => res.end(JSON.stringify({ ok: true, result: [] })), 1_000)
        return
      }
      res.end(JSON.stringify({ ok: true, result: method === "getMe" ? { username: "e2e_bot" } : {} }))
    })
    server.listen(0, "127.0.0.1", () => {
      resolve({ server, port: (server.address() as { port: number }).port })
    })
  })
}

describe("материализация дефа триажа: демон получает промпт из репо", () => {
  const runRoot = mkdtempSync(join(tmpdir(), "triage-defmat-"))
  const noSiyuan = join(runRoot, "no-siyuan") // пустой каталог вместо WS-конфа
  const hubJson = join(REPO, "triage", "hub.json")
  const defDst = join(runRoot, DEF_DST_REL)
  const daemonLog = join(runRoot, ".local", "share", "opencode", "log", "uwuwu-triage.log")
  let proc: ReturnType<typeof Bun.spawn> | undefined
  let mock: MockTg | undefined

  afterAll(() => {
    try { proc?.kill("SIGKILL") } catch { /* уже мёртв */ }
    mock?.server.close()
    try { rmSync(hubJson, { force: true }) } catch {}
    try { rmSync(runRoot, { recursive: true, force: true }) } catch {}
  })

  test("синк дефа на старте: workspace-копия == triage/agent.md репо, без гейтов", async () => {
    mock = await startMockTg()
    // временный кред (gitignored): демон стартует только с token/owner
    writeFileSync(hubJson, JSON.stringify({ token: "tok", owner: "e2e" }))
    mkdirSync(noSiyuan, { recursive: true })
    proc = Bun.spawn(["bun", join(REPO, "triage", "triage.ts")], {
      env: {
        ...process.env,
        UWUWU_TRIAGE_HOME: runRoot,
        UWUWU_TRIAGE_TG_API: `http://127.0.0.1:${mock.port}`,
        UWUWU_TRIAGE_POLL_SEC: "30",
        UWUWU_TRIAGE_WATCH_SEC: "0", // kill-switch вотчера: прод-доски не читаются
        UWUWU_SIYUAN_TOKEN: "",
        UWUWU_SIYUAN_WS: noSiyuan, // пустой каталог → TOKEN "" → вотчер off
      },
      stdout: "ignore",
      stderr: "ignore",
    })
    // syncAgentDef — до ТГ-лупов: файл появляется рано, ждём его
    const deadline = Date.now() + 15_000
    while (!existsSync(defDst)) {
      if (Date.now() > deadline) {
        const log = existsSync(daemonLog) ? readFileSync(daemonLog, "utf-8").slice(-500) : "(нет лога)"
        throw new Error(`деф не материализован за 15с; лог демона:\n${log}`)
      }
      await new Promise((r) => setTimeout(r, 50))
    }
    const materialized = readFileSync(defDst, "utf-8")
    const source = readFileSync(AGENT_MD, "utf-8")
    expect(materialized).toBe(source) // доезжает именно актуальный промпт репо
    for (const s of GATE_SIGNATURES) {
      expect(materialized).not.toContain(s)
    }
  }, 20_000)
})
