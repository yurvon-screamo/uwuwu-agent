// single-instance.test.ts — контракт общего лока демонов (lib/single-instance).
// Спавн-тесты: настоящее межпроцессное поведение (живой владелец, перехват
// мёртвого лока, drain по SIGTERM, чистка на выходе) в процессе не проверишь.
// POSIX-only: SIGTERM-доставка и pid-проверка — линуксовая семантика демонов.
import { describe, expect, test, afterAll } from "bun:test"
import { existsSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { resolveStateDir, sandboxedStateDir } from "../lib/single-instance"

const LIB_PATH = resolve(import.meta.dir, "../lib/single-instance.ts")
const CHILD = join(import.meta.dir, "single-instance-child.ts")

interface Child {
  proc: ReturnType<typeof Bun.spawn>
  dir: string
  stateDir: string
  ready: string
  stopFlag: string
}

/** Ребёнок держит лок и сигналит готовность файлом (handshake: сигналить
 *  SIGTERM раньше захвата — гонка, а не тест). onStop пишет флаг и выходит
 *  сам — drain-паттерн демонов; grace не тестируем, это аварийный путь.
 *  stateDir можно передать чужой — так проверяется отказ второму инстансу. */
async function spawnChild(mode: string, sharedStateDir?: string): Promise<Child> {
  const dir = mkdtempSync(join(tmpdir(), `single-instance-${mode}-`))
  const stateDir = sharedStateDir ?? join(dir, "state")
  const ready = join(dir, "ready")
  const stopFlag = join(dir, "stopped")
  const proc = Bun.spawn(["bun", CHILD, mode, stateDir, ready, stopFlag], {
    env: { ...process.env, LIB_PATH },
    stdout: "pipe",
    stderr: "pipe",
  })
  if (mode === "guard") {
    // отказ не пишет ready — ребёнок выходит 3 сразу после неудачного захвата
    return { proc, dir, stateDir, ready, stopFlag }
  }
  const deadline = Date.now() + 10_000
  while (!existsSync(ready)) {
    if (Date.now() > deadline) throw new Error(`ребёнок ${mode} не захватил лок`)
    await new Promise((r) => setTimeout(r, 50))
  }
  return { proc, dir, stateDir, ready, stopFlag }
}

describe.skipIf(process.platform !== "linux")("single-instance", () => {
  const children: Child[] = []
  afterAll(() => {
    for (const c of children) {
      try { c.proc.kill("SIGKILL") } catch { /* уже мёртв */ }
      try { rmSync(c.dir, { recursive: true, force: true }) } catch {}
    }
  })

  test("второй инстанс при живом владельце — отказ", async () => {
    const a = await spawnChild("hold")
    children.push(a)
    const b = await spawnChild("guard", a.stateDir)
    children.push(b)
    expect(await b.proc.exited).toBe(3)
  })

  test("SIGTERM — onStop вызван, выход 0, лок убран", async () => {
    const a = await spawnChild("hold")
    children.push(a)
    a.proc.kill("SIGTERM")
    expect(await a.proc.exited).toBe(0)
    expect(existsSync(a.stopFlag)).toBeTrue()
    expect(existsSync(join(a.stateDir, "lock"))).toBeFalse()
  })

  test("двойной SIGTERM — onStop один раз, без крэша", async () => {
    // второй сигнал с зазором: синхронные kill коалесцирует ядро, и guard
    // lib не поучаствовал бы; зазор доставляет оба — инвариант держит guard
    const a = await spawnChild("counted")
    children.push(a)
    a.proc.kill("SIGTERM")
    await new Promise((r) => setTimeout(r, 30))
    a.proc.kill("SIGTERM")
    expect(await a.proc.exited).toBe(0)
    const stdout = await new Response(a.proc.stdout).text()
    expect(stdout).toContain("onStop=1")
    expect(stdout).not.toContain("onStop=2")
  })

  test("мёртвый лок (kill -9) перехватывается", async () => {
    const a = await spawnChild("hold")
    children.push(a)
    a.proc.kill("SIGKILL")
    await a.proc.exited
    expect(existsSync(join(a.stateDir, "lock"))).toBeTrue() // stale остался

    // общий stateDir: mkdir lockDir даёт EEXIST от stale, pid мёртв (ESRCH) — ветка перехвата
    const b = await spawnChild("hold", a.stateDir)
    children.push(b)
    expect(existsSync(b.ready)).toBeTrue()
  })

  test("обычный выход — лок убран, следующий захватывает", async () => {
    const a = await spawnChild("quit")
    children.push(a)
    expect(await a.proc.exited).toBe(0)
    expect(existsSync(join(a.stateDir, "lock"))).toBeFalse()

    // общий stateDir: повторный захват чистого каталога
    const b = await spawnChild("hold", a.stateDir)
    children.push(b)
    expect(existsSync(b.ready)).toBeTrue()
  })

  test("exit(1) — лок тоже убран (чистка на exit)", async () => {
    const a = await spawnChild("quit1")
    children.push(a)
    expect(await a.proc.exited).toBe(1)
    expect(existsSync(join(a.stateDir, "lock"))).toBeFalse()
  })

  test("разные stateDir — локи независимы (песочница ≠ прод)", async () => {
    const a = await spawnChild("hold")
    children.push(a)
    const b = await spawnChild("hold")
    children.push(b)
    // оба живы одновременно: песочный лок не видит прод-лок и наоборот
    expect(existsSync(a.ready)).toBeTrue()
    expect(existsSync(b.ready)).toBeTrue()
    expect(existsSync(join(a.stateDir, "lock"))).toBeTrue()
    expect(existsSync(join(b.stateDir, "lock"))).toBeTrue()
  })
})

describe("resolveStateDir / sandboxedStateDir", () => {
  const fallback = "/home/u/.local/state/uwuwu-factory"

  test("env отсутствует/пустой/whitespace — дефолт (прод-путь)", () => {
    expect(resolveStateDir(undefined, fallback)).toBe(fallback)
    expect(resolveStateDir("", fallback)).toBe(fallback)
    expect(resolveStateDir("   ", fallback)).toBe(fallback)
    expect(sandboxedStateDir(undefined)).toBeFalse()
    expect(sandboxedStateDir("")).toBeFalse()
    expect(sandboxedStateDir("  ")).toBeFalse()
  })

  test("env задан — переопределение as-is (песочница)", () => {
    expect(resolveStateDir("/tmp/smoke-factory", fallback)).toBe("/tmp/smoke-factory")
    expect(resolveStateDir("  /tmp/smoke-factory  ", fallback)).toBe("/tmp/smoke-factory")
    expect(sandboxedStateDir("/tmp/smoke-factory")).toBeTrue()
    expect(sandboxedStateDir(" /tmp/smoke-factory ")).toBeTrue()
  })
})
