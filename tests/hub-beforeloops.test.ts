// hub-beforeloops.test.ts — e2e beforeLoops-хука хаба (ADR-076 пиннинг триажа):
// живой демон (child на createHub) против mock-TG. Хук — точка старт-сверки
// демона: throw = отказ до любой поллинг-работы (getMe/лупы не начинаются),
// хук стоит между syncAgentDef и лупами (барьер-сценарий), отсутствие хука —
// прежнее поведение. Юнитами чистых функций не покрывается — это wiring run().
import { describe, test, expect, afterAll } from "bun:test"
import { createServer, type Server } from "node:http"
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "hub-beforeloops-child.ts")

interface MockTg {
  server: Server
  port: number
  /** jsonl журнал вызовов методов TG-API: {method, ts} */
  callsLog: string
}

/** Mock api.telegram.org: каждый вызов метода — строка в jsonl-журнал
 *  (факт = строка), getUpdates «долго-поллит» 1с (не hot-loop). */
function startMockTg(callsLog: string): Promise<MockTg> {
  return new Promise((resolve) => {
    const server = createServer((req, res) => {
      const method = (req.url ?? "").split("/").at(-1) ?? ""
      appendFileSync(callsLog, `${JSON.stringify({ method, ts: Date.now() })}\n`)
      if (method === "getUpdates") {
        setTimeout(() => res.end(JSON.stringify({ ok: true, result: [] })), 1_000)
        return
      }
      res.end(JSON.stringify({ ok: true, result: method === "getMe" ? { username: "e2e_bot" } : {} }))
    })
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as { port: number }).port
      resolve({ server, port, callsLog })
    })
  })
}

const calls = (callsLog: string): { method: string; ts: number }[] =>
  existsSync(callsLog)
    ? readFileSync(callsLog, "utf-8").trim().split("\n").filter((l) => l !== "").map((l) => JSON.parse(l) as { method: string; ts: number })
    : []

const callsOf = (callsLog: string, method: string): { method: string; ts: number }[] =>
  calls(callsLog).filter((c) => c.method === method)

async function waitFor(pred: () => boolean, ms: number, what: string): Promise<void> {
  const deadline = Date.now() + ms
  while (!pred()) {
    if (Date.now() > deadline) throw new Error(`не дождались за ${ms}мс: ${what}`)
    await new Promise((r) => setTimeout(r, 50))
  }
}

/** Демон в песочнице: фикстура (hub.json/agent.md), RUN_ROOT и TG-API — env. */
function spawnChild(mock: MockTg, mode: "throw" | "ok" | "none", dir: string) {
  const fixture = join(dir, "fixture")
  mkdirSync(fixture, { recursive: true })
  writeFileSync(join(fixture, "hub.json"), JSON.stringify({ token: "tok", owner: "e2e" }))
  writeFileSync(join(fixture, "agent.md"), "# e2e def stub\n")
  const runRoot = join(dir, "home")
  mkdirSync(runRoot, { recursive: true })
  const gate = join(dir, "gate")
  const marker = join(dir, "marker")
  const proc = Bun.spawn(["bun", CHILD, fixture, mode, gate, marker], {
    env: {
      ...process.env,
      UWUWU_HUB_BEFORELOOPS_HOME: runRoot,
      UWUWU_HUB_BEFORELOOPS_TG_API: `http://127.0.0.1:${mock.port}`,
    },
    stdout: "ignore",
    stderr: "pipe",
  })
  return { proc, dir, marker, gate }
}

describe("hub beforeLoops e2e (mock-TG)", () => {
  const dir = mkdtempSync(join(tmpdir(), "hub-beforeloops-"))
  const callsLog = join(dir, "tg-calls.jsonl")
  const spawned: ReturnType<typeof spawnChild>[] = []
  let mock: MockTg

  afterAll(() => {
    for (const s of spawned) {
      try { s.proc.kill("SIGKILL") } catch { /* уже мёртв */ }
    }
    mock?.server.close()
    try { rmSync(dir, { recursive: true, force: true }) } catch {}
  })

  test("throw хука — отказ старта до поллинга: exit≠0, getMe/лупы не начались", async () => {
    mock = await startMockTg(callsLog)
    const s = spawnChild(mock, "throw", join(dir, "throw"))
    const [, stderr] = await Promise.all([new Response(s.proc.stdout).text(), new Response(s.proc.stderr).text()])
    const code = await s.proc.exited
    expect(code).toBe(1)
    expect(stderr).toContain("пиннинг-хук e2e: отказ старта до любой поллинг-работы")
    // отказ до любой поллинг-работы: ни getMe, ни getUpdates мок не увидел
    expect(calls(callsLog)).toEqual([])
  })

  test("ok хук — барьер: лупы не стартуют до отпуска хука, маркер до getMe", async () => {
    const s = spawnChild(mock, "ok", join(dir, "ok"))
    spawned.push(s)
    // хук держит старт: getMe ещё не было (хук между syncAgentDef и лупами)
    await new Promise((r) => setTimeout(r, 1_500))
    expect(existsSync(s.marker)).toBe(false)
    expect(callsOf(callsLog, "getMe")).toEqual([])
    // отпускаем хук: маркер (хук прошёл) → затем getMe (старт-строка/лупы)
    writeFileSync(s.gate, "1")
    await waitFor(() => existsSync(s.marker), 15_000, "маркер хука после gate")
    await waitFor(() => callsOf(callsLog, "getMe").length > 0, 15_000, "getMe после хука")
    expect(callsOf(callsLog, "getMe").length).toBeGreaterThan(0)
    expect(callsOf(callsLog, "getMe").at(0)?.ts).toBeGreaterThanOrEqual(Number(readFileSync(s.marker, "utf-8")))
    expect(s.proc.exitCode).toBeNull() // лупы живы — демон не упал
  })

  test("хук не задан — прежнее поведение: старт без маркера", async () => {
    const s = spawnChild(mock, "none", join(dir, "none"))
    spawned.push(s)
    await waitFor(() => callsOf(callsLog, "getMe").length > 0, 15_000, "getMe без хука")
    expect(existsSync(s.marker)).toBe(false)
    expect(s.proc.exitCode).toBeNull()
  })
})
