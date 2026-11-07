// hub-silence-e2e.test.ts — e2e алерт-пути тайм-аута тишины: живой демон
// (child на createHub) против mock-TG. Интеграционный контур — материализация
// метки при загрузке state, wiring silenceLoop, доставка алерта в залоченный
// chatID — юнитами silenceCheck не покрывается (класс бага-инцидента 07.11
// был интеграционным). Сценарии: состаренная lastDeliveryAt в state.json
// песочницы → алерт на первом тике; свежая метка → тишина.
import { describe, test, expect, afterAll } from "bun:test"
import { createServer, type Server } from "node:http"
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "hub-silence-child.ts")
const TIMEOUT_MS = 10_000 // тестовый порог тишины (атрибут профиля ребёнка)
const CHAT_ID = 4242

interface MockTg {
  server: Server
  port: number
  sentLog: string
}

/** Mock api.telegram.org: getUpdates «долго-поллит» 1с (не hot-loop),
 *  sendMessage пишет тело POST в jsonl-журнал (факт доставки = строка). */
function startMockTg(sentLog: string): Promise<MockTg> {
  return new Promise((resolve) => {
    const server = createServer((req, res) => {
      const method = (req.url ?? "").split("/").at(-1) ?? ""
      let body = ""
      req.on("data", (c: Buffer) => { body += c })
      req.on("end", () => {
        if (method === "sendMessage") {
          appendFileSync(sentLog, `${body}\n`)
          res.end(JSON.stringify({ ok: true, result: { message_id: 1 } }))
          return
        }
        if (method === "getUpdates") {
          setTimeout(() => res.end(JSON.stringify({ ok: true, result: [] })), 1_000)
          return
        }
        res.end(JSON.stringify({ ok: true, result: method === "getMe" ? { username: "e2e_bot" } : {} }))
      })
    })
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as { port: number }).port
      resolve({ server, port, sentLog })
    })
  })
}

interface Sent {
  text: string
  chat_id: number
}

const sentMessages = (sentLog: string): Sent[] =>
  existsSync(sentLog)
    ? readFileSync(sentLog, "utf-8").trim().split("\n").filter((l) => l !== "").map((l) => JSON.parse(l) as Sent)
    : []

interface Demon {
  proc: ReturnType<typeof Bun.spawn>
  dir: string
  daemonLog: string
  sentLog: string
}

/** Демон в песочнице: фикстура (hub.json/agent.md) + state.json с заданной
 *  меткой тишины, RUN_ROOT и TG-API — env. Готовность — файл ready после
 *  hub.run() (лупы стартовали, первый тик тишины уже отработал или в полёте). */
async function spawnDemon(mock: MockTg, lastDeliveryAt: number): Promise<Demon> {
  const dir = mkdtempSync(join(tmpdir(), "hub-silence-e2e-"))
  const fixture = join(dir, "fixture")
  mkdirSync(fixture, { recursive: true })
  writeFileSync(join(fixture, "hub.json"), JSON.stringify({ token: "tok", owner: "e2e" }))
  writeFileSync(join(fixture, "agent.md"), "# e2e def stub\n")
  const runRoot = join(dir, "home")
  const stateDir = join(runRoot, ".local/state/hub-silence-e2e")
  mkdirSync(stateDir, { recursive: true })
  writeFileSync(join(stateDir, "state.json"), JSON.stringify({ offset: 0, chatID: CHAT_ID, queue: [], lastDeliveryAt }))
  const ready = join(dir, "ready")
  const daemonLog = join(runRoot, ".local/share/opencode/log/hub-silence-e2e.log")
  const proc = Bun.spawn(["bun", CHILD, fixture, ready, String(TIMEOUT_MS)], {
    env: {
      ...process.env,
      UWUWU_HUB_SILENCE_HOME: runRoot,
      UWUWU_HUB_SILENCE_TG_API: `http://127.0.0.1:${mock.port}`,
    },
    stdout: "ignore",
    stderr: "inherit",
  })
  const deadline = Date.now() + 15_000
  while (!existsSync(ready)) {
    if (Date.now() > deadline) {
      throw new Error(`демон не стартовал (exit=${proc.exitCode}); лог:\n${existsSync(daemonLog) ? readFileSync(daemonLog, "utf-8") : "(нет)"}`)
    }
    await new Promise((r) => setTimeout(r, 50))
  }
  return { proc, dir, daemonLog, sentLog: mock.sentLog }
}

describe("hub silence e2e (mock-TG)", () => {
  const dir = mkdtempSync(join(tmpdir(), "hub-silence-mock-"))
  const sentLog = join(dir, "sent.jsonl")
  const demons: Demon[] = []
  let mock: MockTg

  afterAll(() => {
    for (const d of demons) {
      try { d.proc.kill("SIGKILL") } catch { /* уже мёртв */ }
    }
    mock?.server.close()
    try { rmSync(dir, { recursive: true, force: true }) } catch {}
    for (const d of demons) {
      try { rmSync(d.dir, { recursive: true, force: true }) } catch {}
    }
  })

  test("состаренная lastDeliveryAt → алерт в залоченный чат на первом тике", async () => {
    mock = await startMockTg(sentLog)
    const before = sentMessages(sentLog).length
    const d = await spawnDemon(mock, Date.now() - 60_000)
    demons.push(d)
    const deadline = Date.now() + 20_000
    while (sentMessages(sentLog).length === before) {
      if (Date.now() > deadline) {
        throw new Error(`алерт не доставлен за 20с; лог демона:\n${existsSync(d.daemonLog) ? readFileSync(d.daemonLog, "utf-8") : "(нет)"}`)
      }
      await new Promise((r) => setTimeout(r, 100))
    }
    const msgs = sentMessages(sentLog).slice(before)
    expect(msgs.length).toBe(1)
    expect(msgs[0]!.chat_id).toBe(CHAT_ID)
    expect(msgs[0]!.text).toContain("тишина выходного потока")
    // метка персистится: успешная доставка — выход, state.json уже с ней
    // (follow-up тик не должен задублировать алерт — часы сброшены)
    await new Promise((r) => setTimeout(r, 1_500))
    expect(sentMessages(sentLog).slice(before).length).toBe(1)
  }, 30_000)

  test("свежая метка → тишина, алертов нет", async () => {
    const before = sentMessages(sentLog).length
    const d = await spawnDemon(mock, Date.now())
    demons.push(d)
    // наблюдение меньше порога (10с): первый тик — age≈0, второй тик за 60с —
    // за пределами окна; за 6с алертов быть не должно
    await new Promise((r) => setTimeout(r, 6_000))
    expect(sentMessages(sentLog).slice(before)).toEqual([])
  }, 20_000)
})
