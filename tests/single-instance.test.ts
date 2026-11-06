// single-instance.test.ts — контракт общего лока демонов (lib/single-instance).
// Спавн-тесты: настоящее межпроцессное поведение (живой владелец, перехват
// мёртвого лока, drain по SIGTERM, чистка на выходе) в процессе не проверишь.
// POSIX-only: SIGTERM-доставка и pid-проверка — линуксовая семантика демонов.
import { describe, expect, test, afterAll } from "bun:test"
import { existsSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { resolveStateDir, resolveDaemonStateDir, branchLabel, daemonLogPath, emitNotice, type DaemonStateResolution } from "../lib/single-instance"

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
    const stdout = await new Response(a.proc.stdout as ReadableStream<Uint8Array>).text()
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

describe("resolveStateDir", () => {
  const fallback = "/home/u/.local/state/uwuwu-factory"

  test("env отсутствует/пустой/whitespace — дефолт (прод-путь)", () => {
    expect(resolveStateDir(undefined, fallback)).toBe(fallback)
    expect(resolveStateDir("", fallback)).toBe(fallback)
    expect(resolveStateDir("   ", fallback)).toBe(fallback)
  })

  test("env задан — переопределение as-is (песочница)", () => {
    expect(resolveStateDir("/tmp/smoke-factory", fallback)).toBe("/tmp/smoke-factory")
    expect(resolveStateDir("  /tmp/smoke-factory  ", fallback)).toBe("/tmp/smoke-factory")
  })
})

describe("branchLabel / daemonLogPath / emitNotice", () => {
  test("branchLabel: null-детект → null, пустой stdout → detached, trim", () => {
    expect(branchLabel(null)).toBeNull()
    expect(branchLabel("")).toBe("detached")
    expect(branchLabel("   ")).toBe("detached")
    expect(branchLabel("  master  ")).toBe("master")
    expect(branchLabel("work/task-1")).toBe("work/task-1")
  })

  test("daemonLogPath: override выигрывает; песочница — state-каталог; прод — логи opencode", () => {
    expect(daemonLogPath({ override: "/tmp/x.log", stateDir: "/s", sandboxed: false, fileName: "d.log", homeDir: "/h" })).toBe("/tmp/x.log")
    expect(daemonLogPath({ override: undefined, stateDir: "/s", sandboxed: true, fileName: "d.log", homeDir: "/h" })).toBe("/s/d.log")
    expect(daemonLogPath({ override: undefined, stateDir: "/s", sandboxed: false, fileName: "d.log", homeDir: "/h" })).toBe("/h/.local/share/opencode/log/d.log")
  })

  test("emitNotice: null — тихо; строка — console.error + log", () => {
    const lines: string[] = []
    const orig = console.error
    console.error = (m: string): void => { lines.push(`err:${m}`) }
    try {
      emitNotice(null, (m) => lines.push(`log:${m}`))
      expect(lines).toEqual([])
      emitNotice("notice!", (m) => lines.push(`log:${m}`))
      expect(lines).toEqual(["err:notice!", "log:notice!"])
    } finally {
      console.error = orig
    }
  })
})

describe("resolveDaemonStateDir — дефолт-изоляция веточных чекаутов", () => {
  const base = {
    daemon: "uwuwu-factory",
    envName: "UWUWU_FACTORY_STATE_DIR",
    defaultDir: "/home/u/.local/state/uwuwu-factory",
    repoRoot: "/home/u/repo",
  }
  const detectOf = (out: string | null) => () => out
  const resolveWith = (detectOut: string | null, envRaw?: string): DaemonStateResolution =>
    resolveDaemonStateDir({ ...base, envRaw, detect: detectOf(detectOut) })

  test("явный env выигрывает — git-детект не зовётся (песочница as-is)", () => {
    let called = false
    const r = resolveDaemonStateDir({
      ...base,
      envRaw: "  /tmp/smoke-factory  ",
      detect: () => { called = true; return "work/task-1" },
    })
    expect(r).toEqual({ stateDir: "/tmp/smoke-factory", sandboxed: true, autoIsolated: false, notice: null })
    expect(called).toBeFalse()
  })

  test("master — прод-дефолт, тихо (прежнее поведение прод-демона)", () => {
    expect(resolveWith("master")).toEqual({
      stateDir: base.defaultDir, sandboxed: false, autoIsolated: false, notice: null,
    })
    // whitespace в stdout git обрезается
    expect(resolveWith("  master  ")).toEqual({
      stateDir: base.defaultDir, sandboxed: false, autoIsolated: false, notice: null,
    })
  })

  test("не-master ветка — авто-изоляция branches/<safe-branch> + громкий notice", () => {
    const r = resolveWith("work/task-20261106082216-lkgrqwh")
    expect(r.stateDir).toBe(join(base.defaultDir, "branches", "work_task-20261106082216-lkgrqwh"))
    expect(r.sandboxed).toBeTrue()
    expect(r.autoIsolated).toBeTrue()
    expect(r.notice).toContain("work/task-20261106082216-lkgrqwh")
    expect(r.notice).toContain("UWUWU_FACTORY_STATE_DIR")
    expect(r.notice).toContain("не master")
  })

  test("detached HEAD (пустой stdout) — изоляция branches/detached", () => {
    const r = resolveWith("")
    expect(r.stateDir).toBe(join(base.defaultDir, "branches", "detached"))
    expect(r.autoIsolated).toBeTrue()
  })

  test("git-детект не удался — прод-дефолт + громкий warning (не изоляция)", () => {
    const r = resolveWith(null)
    expect(r.stateDir).toBe(base.defaultDir)
    expect(r.sandboxed).toBeFalse()
    expect(r.autoIsolated).toBeFalse()
    expect(r.notice).toContain("git-детект")
    expect(r.notice).toContain("UWUWU_FACTORY_STATE_DIR")
  })

  test("имя ветки санитизируется в сегмент пути (слэши/двоеточия/пробелы)", () => {
    expect(resolveWith("feat: x y").stateDir).toBe(join(base.defaultDir, "branches", "feat__x_y"))
    expect(resolveWith("release/1.2").stateDir).toBe(join(base.defaultDir, "branches", "release_1.2"))
  })
})
