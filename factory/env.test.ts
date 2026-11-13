// env.test.ts — интеграционные тесты окружения задачи на реальном temp-git-репо.
// Без сети: base = локальная ветка master тестового репо (fetch origin честно
// падает и покрывает best-effort ветку). FS — managed-зависимость, реальные
// временные каталоги на каждый тест.
import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, utimesSync, existsSync, readdirSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { cleanupEnv, ensureEnv, envPaths, allocateDisplayNumber, displayFilePath, readDisplayNumber, sanitizeRowId, sweepEnvs, type EnvDeps } from "./env"

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

describe("Xvfb-дисплей: лиз-файл и аллокация (задача 20261112213522-hxepjy4)", () => {
  const range = { start: 100, end: 399 }
  // изолированный env-root на кейс: envRoot() общий на describe, соседние env
  // других тестов уже держат лизы — номера дисплеев были бы не предсказуемы
  const displayRoot = (): string => mkdtempSync(join(tmpdir(), "uwuwu-display-"))
  /** пустой temp-x11Dir для ensureEnv-кейсов: без инъекции ensureEnv сканирует
   *  прод-константу /tmp/.X11-unix — живой Xvfb машины (штатный режим фичи)
   *  сдвигал бы пины номеров, тесты флеймили (баг-репорт тестировщика, PR #244) */
  const emptyX11 = (root: string): string => {
    const dir = join(root, "x11")
    mkdirSync(dir, { recursive: true })
    return dir
  }
  const mkX11 = (root: string, sockets: number[]): string => {
    const dir = join(root, "x11")
    mkdirSync(dir, { recursive: true })
    for (const n of sockets) writeFileSync(join(dir, `X${n}`), "")
    return dir
  }
  describe("allocateDisplayNumber", () => {
    test("пустой root и без сокетов — наименьший свободный range.start", () => {
      const root = mkdtempSync(join(tmpdir(), "uwuwu-alloc-"))
      try {
        const n = allocateDisplayNumber({ envRoot: root, selfRowId: "r1", x11Dir: join(root, "x11"), range })
        expect(n).toBe(100)
      } finally {
        rmSync(root, { recursive: true, force: true })
      }
    })
    test("соседние env-файлы и X-сокеты занимают номера — наименьший свободный с перескоком", () => {
      const root = mkdtempSync(join(tmpdir(), "uwuwu-alloc-"))
      try {
        mkdirSync(join(root, "neighbor-a"), { recursive: true })
        writeFileSync(join(root, "neighbor-a", "display"), "100")
        mkdirSync(join(root, "neighbor-b"), { recursive: true })
        writeFileSync(join(root, "neighbor-b", "display"), "102")
        const x11 = mkX11(root, [101])
        const n = allocateDisplayNumber({ envRoot: root, selfRowId: "self", x11Dir: x11, range })
        expect(n).toBe(103)
      } finally {
        rmSync(root, { recursive: true, force: true })
      }
    })
    test(".attic и свой каталог не сканируются как соседи", () => {
      const root = mkdtempSync(join(tmpdir(), "uwuwu-alloc-"))
      try {
        mkdirSync(join(root, ".attic", "old"), { recursive: true })
        writeFileSync(join(root, ".attic", "old", "display"), "100")
        // свой env (selfRowId) уже может нести display — не считаем себя чужим
        mkdirSync(join(root, "self"), { recursive: true })
        writeFileSync(join(root, "self", "display"), "100")
        const n = allocateDisplayNumber({ envRoot: root, selfRowId: "self", x11Dir: join(root, "x11"), range })
        expect(n).toBe(100)
      } finally {
        rmSync(root, { recursive: true, force: true })
      }
    })
    test("насыщение диапазона — null (деградация без throw)", () => {
      const root = mkdtempSync(join(tmpdir(), "uwuwu-alloc-"))
      try {
        const sockets: number[] = []
        for (let n = range.start; n <= range.end; n++) sockets.push(n)
        const x11 = mkX11(root, sockets)
        const n = allocateDisplayNumber({ envRoot: root, selfRowId: "r1", x11Dir: x11, range })
        expect(n).toBeNull()
      } finally {
        rmSync(root, { recursive: true, force: true })
      }
    })
  })
  test("readDisplayNumber: голый номер парсится, мусор/отсутствие — null", () => {
    const root = mkdtempSync(join(tmpdir(), "uwuwu-disp-"))
    try {
      writeFileSync(join(root, "ok"), "123")
      expect(readDisplayNumber(join(root, "ok"))).toBe(123)
      writeFileSync(join(root, "colon"), ":123")
      expect(readDisplayNumber(join(root, "colon"))).toBeNull()
      writeFileSync(join(root, "junk"), "abc\n456")
      expect(readDisplayNumber(join(root, "junk"))).toBeNull()
      expect(readDisplayNumber(join(root, "missing"))).toBeNull()
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("ensureEnv пишет лиз-файл с голым номером, reuse держит номер", async () => {
    const root = displayRoot()
    try {
      const first = await ensureEnv(deps, { root, rowId: "disprow1", local: baseRoot, base: "master", x11Dir: emptyX11(root) })
      expect(first.display).toBe(100)
      expect(readDisplayNumber(displayFilePath(first.envDir))).toBe(100)
      expect(readFileSync(displayFilePath(first.envDir), "utf-8")).toBe("100")
      const second = await ensureEnv(deps, { root, rowId: "disprow1", local: baseRoot, base: "master" })
      expect(second.display).toBe(100)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("сосед с :100 — новый env берёт :101 (занятость соседей)", async () => {
    const root = displayRoot()
    try {
      await ensureEnv(deps, { root, rowId: "disprow2", local: baseRoot, base: "master", x11Dir: emptyX11(root) })
      const p = await ensureEnv(deps, { root, rowId: "disprow3", local: baseRoot, base: "master", x11Dir: emptyX11(root) })
      expect(p.display).toBe(101)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("живой X-сокет чужого дисплея — перескок (человеческие/чужие не трогаем)", async () => {
    const root = displayRoot()
    try {
      // прод-путь /tmp/.X11-unix в тестах не трогаем: ensureEnv использует
      // прод-константу, кейс гоняет аллокатор напрямую (контракт занятости тот же)
      const x11 = mkX11(root, [100])
      const n = allocateDisplayNumber({ envRoot: root, selfRowId: "r1", x11Dir: x11, range })
      expect(n).toBe(101)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("display-файл с мусором — пере-аллокация с громким логом", async () => {
    const root = displayRoot()
    try {
      mkdirSync(join(root, "disprow4"), { recursive: true })
      writeFileSync(displayFilePath(join(root, "disprow4")), ":42")
      const p = await ensureEnv(deps, { root, rowId: "disprow4", local: baseRoot, base: "master", x11Dir: emptyX11(root) })
      expect(p.display).not.toBeNull()
      expect(logs.some((l) => l.includes("disprow4") && l.includes("пере-аллокация"))).toBe(true)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("ensureEnv учитывает x11Dir-инъекцию: живой X-сокет temp-каталога сдвигает номер", async () => {
    const root = displayRoot()
    try {
      const x11 = mkX11(root, [100])
      const p = await ensureEnv(deps, { root, rowId: "injrow1", local: baseRoot, base: "master", x11Dir: x11 })
      expect(p.display).toBe(101)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("конкурентная волна: два ensureEnv в одном тике — разные номера (TOCTOU)", async () => {
    const root = displayRoot()
    try {
      // resolveDisplay синхронен и стоит до первого await ensureEnv — event loop
      // сериализует участок read→write; если аллокация уедет после await,
      // оба вызова прочитают одинаковых соседей и возьмут один номер
      const [a, b] = await Promise.all([
        ensureEnv(deps, { root, rowId: "wave1", local: baseRoot, base: "master", x11Dir: emptyX11(root) }),
        ensureEnv(deps, { root, rowId: "wave2", local: baseRoot, base: "master", x11Dir: emptyX11(root) }),
      ])
      expect(a.display).not.toBeNull()
      expect(b.display).not.toBeNull()
      expect(a.display).not.toBe(b.display)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
})

describe("ensureEnv: main-репа — base-контракт ProjectCfg.base (класс 1xgames/GitLab-notix)", () => {
  let mainRoot = ""
  beforeAll(async () => {
    // main-based репа с локальным bare origin (без сети): git-факты те же,
    // что у remote main-репо
    const origin = join(baseRoot, "origin-main.git")
    await sh(["git", "init", "--bare", "-b", "main", origin])
    mainRoot = join(baseRoot, "main-work")
    await sh(["git", "init", "-b", "main", mainRoot])
    writeFileSync(join(mainRoot, "svc.txt"), "svc\n")
    await sh(["git", "-C", mainRoot, "add", "."])
    await sh(["git", "-C", mainRoot, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "svc"])
    await sh(["git", "-C", mainRoot, "remote", "add", "origin", origin])
    await sh(["git", "-C", mainRoot, "push", "-u", "origin", "main"])
  })

  test("base=origin/main: env-ворктри создаётся от remote-main, ветка по row-id", async () => {
    const p = await ensureEnv(deps, { root: envRoot(), rowId: "mainrow1", local: mainRoot, base: "origin/main" })
    expect(await sh(["git", "-C", p.repoDir, "branch", "--show-current"])).toBe("work/task-mainrow1")
    expect(readFileSync(join(p.repoDir, "svc.txt"), "utf-8")).toContain("svc")
    // HEAD ворктри = origin/main, не «первый попавшийся» ref
    expect(await sh(["git", "-C", p.repoDir, "rev-parse", "HEAD"]))
      .toBe(await sh(["git", "-C", mainRoot, "rev-parse", "origin/main"]))
  })

  test("base=origin/master на main-репе: честный отказ (репро «invalid reference» хардкода до фикса)", async () => {
    await expect(
      ensureEnv(deps, { root: envRoot(), rowId: "mainrow2", local: mainRoot, base: "origin/master" }),
    ).rejects.toThrow(/invalid reference/)
    expect(logs.some((l) => l.includes("mainrow2") && l.includes("worktree add -b не удался"))).toBe(true)
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
