// env.test.ts — интеграционные тесты окружения задачи на реальном temp-git-репо.
// Без сети: base = локальная ветка master тестового репо (fetch origin честно
// падает и покрывает best-effort ветку). FS — managed-зависимость, реальные
// временные каталоги на каждый тест.
import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, utimesSync, existsSync, readdirSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { cleanupEnv, ensureEnv, envPaths, sanitizeRowId, sweepEnvs, type EnvDeps } from "./env"

const sh = async (cmd: string[], cwd?: string): Promise<string> => {
  const proc = Bun.spawn({ cmd, cwd, stdout: "pipe", stderr: "pipe" })
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  const code = await proc.exited
  if (code !== 0) throw new Error(`${cmd.join(" ")} (${code}): ${(err.trim() || out.trim()).slice(0, 300)}`)
  return out.trim()
}

const logs: string[] = []
const deps: EnvDeps = { run: sh, log: (m) => logs.push(m) }

let baseRoot = ""

beforeAll(async () => {
  baseRoot = mkdtempSync(join(tmpdir(), "uwuwu-env-test-"))
  // «главный чекаут»: git-репо с коммитом на master — база ворктри
  writeFileSync(join(baseRoot, "seed.txt"), "seed\n")
  await sh(["git", "-C", baseRoot, "init", "-b", "master"])
  await sh(["git", "-C", baseRoot, "add", "."])
  await sh(["git", "-C", baseRoot, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "seed"])
})

afterAll(() => {
  rmSync(baseRoot, { recursive: true, force: true })
})

const envRoot = (): string => join(baseRoot, "agents")

describe("sanitizeRowId", () => {
  test("ведущий дефис и мусор вырезаются, пустой id → task", () => {
    expect(sanitizeRowId("-5BF9NMBXZ")).toBe("5BF9NMBXZ")
    expect(sanitizeRowId("20261104212512-ms8lqhg")).toBe("20261104212512-ms8lqhg")
    expect(sanitizeRowId("abc$#@!def")).toBe("abcdef")
    expect(sanitizeRowId("---")).toBe("task")
  })
})

describe("envPaths", () => {
  test("раскладка env и ветка из ПОЛНОГО row-id", () => {
    const p = envPaths("/root", "20261104212512-ms8lqhg")
    expect(p.envDir).toBe("/root/20261104212512-ms8lqhg")
    expect(p.repoDir).toBe("/root/20261104212512-ms8lqhg/repo")
    expect(p.scratchDir).toBe("/root/20261104212512-ms8lqhg/scratch")
    expect(p.logsDir).toBe("/root/20261104212512-ms8lqhg/logs")
    expect(p.branch).toBe("work/task-20261104212512-ms8lqhg")
  })
  test("row-id одной секунды — разные ветки (нет коллизии префикса)", () => {
    // две задачи, созданные в одну секунду, различаются хвостом row-id:
    // 12-символьный префикс коллидировал бы в одной ветке
    const a = envPaths("/root", "20261104212512-ms8lqhg")
    const b = envPaths("/root", "20261104212512-z9x2vbn4")
    expect(a.branch).not.toBe(b.branch)
    expect(a.envDir).not.toBe(b.envDir)
  })
})

describe("ensureEnv — реальный git", () => {
  test("создаёт раскладку, ворктри на нужной ветке", async () => {
    const root = envRoot()
    const p = await ensureEnv(deps, { root, rowId: "row1", local: baseRoot, base: "master" })
    expect(existsSync(p.repoDir)).toBe(true)
    expect(existsSync(p.scratchDir)).toBe(true)
    expect(existsSync(p.logsDir)).toBe(true)
    expect(await sh(["git", "-C", p.repoDir, "branch", "--show-current"])).toBe("work/task-row1")
    expect(readFileSync(join(p.repoDir, "seed.txt"), "utf-8")).toContain("seed")
  })

  test("второй вызов — reuse без дублей регистрации", async () => {
    const root = envRoot()
    const first = await ensureEnv(deps, { root, rowId: "row2", local: baseRoot, base: "master" })
    const second = await ensureEnv(deps, { root, rowId: "row2", local: baseRoot, base: "master" })
    expect(second).toEqual(first)
    const list = await sh(["git", "-C", baseRoot, "worktree", "list", "--porcelain"])
    expect(list.split("\n").filter((l) => l === `worktree ${first.repoDir}`)).toHaveLength(1)
  })

  test("ветка уже существует (ретрай после частичного фейла) — attach, не фейл", async () => {
    const root = envRoot()
    await sh(["git", "-C", baseRoot, "branch", "work/task-row3", "master"])
    const p = await ensureEnv(deps, { root, rowId: "row3", local: baseRoot, base: "master" })
    expect(await sh(["git", "-C", p.repoDir, "branch", "--show-current"])).toBe("work/task-row3")
  })

  test("обломок ворктри без регистрации — сносится и пересоздаётся", async () => {
    const root = envRoot()
    const stale = join(root, "row4", "repo")
    mkdirSync(stale, { recursive: true })
    writeFileSync(join(stale, "junk.txt"), "junk")
    const p = await ensureEnv(deps, { root, rowId: "row4", local: baseRoot, base: "master" })
    expect(existsSync(p.repoDir)).toBe(true)
    expect(existsSync(join(p.repoDir, ".git"))).toBe(true)
    expect(logs.some((l) => l.includes("row4") && l.includes("без регистрации"))).toBe(true)
  })
})

describe("cleanupEnv — реальный git", () => {
  test("чистое окружение: rm → prune → branch -d (порядок, ветка удалена)", async () => {
    const root = envRoot()
    const p = await ensureEnv(deps, { root, rowId: "rowc1", local: baseRoot, base: "master" })
    const outcome = await cleanupEnv(deps, { root, rowId: "rowc1", local: baseRoot, envDir: p.envDir, branch: p.branch })
    expect(outcome).toBe("removed")
    expect(existsSync(p.envDir)).toBe(false)
    const branches = await sh(["git", "-C", baseRoot, "branch", "--list", "work/task-rowc1"])
    expect(branches).toBe("")
    const list = await sh(["git", "-C", baseRoot, "worktree", "list", "--porcelain"])
    expect(list).not.toContain(p.repoDir)
  })

  test("тяжёлый игнорируемый каталог при чистом porcelain умирает вместе с env", async () => {
    const root = envRoot()
    const p = await ensureEnv(deps, { root, rowId: "rowc2", local: baseRoot, base: "master" })
    writeFileSync(join(p.repoDir, ".gitignore"), "target/\n")
    await sh(["git", "-C", p.repoDir, "add", ".gitignore"])
    await sh(["git", "-C", p.repoDir, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "ignore"])
    mkdirSync(join(p.repoDir, "target", "debug"), { recursive: true })
    writeFileSync(join(p.repoDir, "target", "debug", "o.bin"), "x")
    const outcome = await cleanupEnv(deps, { root, rowId: "rowc2", local: baseRoot, envDir: p.envDir, branch: p.branch })
    expect(outcome).toBe("removed")
    expect(existsSync(p.envDir)).toBe(false)
  })

  test("грязное окружение: attic без target/node_modules, изменённый файл жив", async () => {
    const root = envRoot()
    const p = await ensureEnv(deps, { root, rowId: "rowc3", local: baseRoot, base: "master" })
    writeFileSync(join(p.repoDir, "seed.txt"), "changed\n")
    mkdirSync(join(p.repoDir, "target"), { recursive: true })
    writeFileSync(join(p.repoDir, "target", "big.bin"), "x")
    mkdirSync(join(p.repoDir, "node_modules"), { recursive: true })
    writeFileSync(join(p.repoDir, "node_modules", "dep.js"), "x")
    const outcome = await cleanupEnv(deps, { root, rowId: "rowc3", local: baseRoot, envDir: p.envDir, branch: p.branch })
    expect(outcome).toBe("atticked")
    const atticEntries = readdirSync(join(root, ".attic")).filter((e) => e.startsWith("rowc3"))
    expect(atticEntries).toHaveLength(1)
    const atticRepo = join(root, ".attic", atticEntries[0] ?? "", "repo")
    expect(readFileSync(join(atticRepo, "seed.txt"), "utf-8")).toContain("changed")
    expect(existsSync(join(atticRepo, "target"))).toBe(false)
    expect(existsSync(join(atticRepo, "node_modules"))).toBe(false)
  })

  test("несмерженная ветка branch -d не берёт — warn, ветка остаётся", async () => {
    const root = envRoot()
    const p = await ensureEnv(deps, { root, rowId: "rowc4", local: baseRoot, base: "master" })
    writeFileSync(join(p.repoDir, "ahead.txt"), "x")
    await sh(["git", "-C", p.repoDir, "add", "."])
    await sh(["git", "-C", p.repoDir, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "ahead"])
    const outcome = await cleanupEnv(deps, { root, rowId: "rowc4", local: baseRoot, envDir: p.envDir, branch: p.branch })
    expect(outcome).toBe("removed")
    const branches = await sh(["git", "-C", baseRoot, "branch", "--list", "work/task-rowc4"])
    expect(branches).toContain("work/task-rowc4")
    expect(logs.some((l) => l.includes("rowc4") && l.includes("не удалена"))).toBe(true)
  })
})

describe("sweepEnvs — старт-свип", () => {
  test("сирота старше порога сносится, живая и свежая — нет; старый attic сносится", () => {
    const root = join(baseRoot, "sweep")
    mkdirSync(join(root, "orphan-old"), { recursive: true })
    mkdirSync(join(root, "orphan-fresh"), { recursive: true })
    mkdirSync(join(root, "alive"), { recursive: true })
    mkdirSync(join(root, ".attic", "attic-old"), { recursive: true })
    const old = new Date(Date.now() - 8 * 24 * 60 * 60 * 1000)
    utimesSync(join(root, "orphan-old"), old, old)
    utimesSync(join(root, ".attic", "attic-old"), old, old)
    const result = sweepEnvs(deps, {
      root,
      activeRowIds: new Set(["alive"]),
      nowMs: Date.now(),
      maxAgeMs: 7 * 24 * 60 * 60 * 1000,
    })
    expect(result.sweptEnvs).toEqual(["orphan-old"])
    expect(result.sweptAttic).toEqual(["attic-old"])
    expect(existsSync(join(root, "orphan-fresh"))).toBe(true)
    expect(existsSync(join(root, "alive"))).toBe(true)
    expect(existsSync(join(root, ".attic", "attic-old"))).toBe(false)
  })
})
