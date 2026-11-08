// worktree-gc.test.ts — интеграционные тесты GC легаси-ворктри на реальном
// temp-git-репо. Без сети: base = локальная ветка master тестового репо
// (fetch origin честно падает и покрывает best-effort ветку), gh — фейковый
// фиксчер через deps (unmanaged-зависимость → mock по правилам тест-скилла).
import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { gcLegacyWorktrees, registeredWorktrees, type GcArgs, type GcDeps } from "./worktree-gc"

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

const GRACE_MS = 24 * 60 * 60 * 1000
const HOUR_MS = 60 * 60 * 1000

const roots: string[] = []
const logs: string[] = []

/** фейковый gh: фиксчер ответа или управляемый сбой; git — реальный */
let ghResponse: string | null
let ghShouldFail = false

const deps: GcDeps = {
  run: async (cmd, cwd) => {
    if (cmd[0] === "gh") {
      if (ghShouldFail) throw new Error("gh (1): simulated failure")
      if (ghResponse === null) throw new Error("gh fixture missing")
      return ghResponse
    }
    return sh(cmd, cwd)
  },
  log: (m) => logs.push(m),
}

interface TestRepo {
  root: string
  local: string
  wtRoot: string
  legacyRoot: string
}

async function makeRepo(): Promise<TestRepo> {
  const root = mkdtempSync(join(tmpdir(), "uwuwu-gc-test-"))
  roots.push(root)
  const local = join(root, "checkout")
  mkdirSync(local)
  writeFileSync(join(local, "seed.txt"), "seed\n")
  await sh(["git", "-C", local, "init", "-b", "master"])
  // identity локально в temp-репо: без неё merge падает на машинах
  // без глобального git-конфига (CI-раннеры)
  await sh(["git", "-C", local, "config", "user.name", "t"])
  await sh(["git", "-C", local, "config", "user.email", "t@t"])
  await sh(["git", "-C", local, "add", "."])
  await sh(["git", "-C", local, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "seed"])
  return { root, local, wtRoot: join(local, "wt"), legacyRoot: join(local, ".worktrees") }
}

/** merged-ветка: ворктри в корне, коммит в нём, --no-ff мердж в master.
 *  worktree остаётся чистым (проверки GC проходят). */
async function mergedBranchWorktree(
  repo: TestRepo,
  branch: string,
  subdir: "wt" | ".worktrees",
): Promise<string> {
  const rootDir = subdir === "wt" ? repo.wtRoot : repo.legacyRoot
  const wtPath = join(rootDir, branch.replaceAll("/", "-"))
  await sh(["git", "-C", repo.local, "worktree", "add", wtPath, "-b", branch])
  writeFileSync(join(wtPath, `${branch.replaceAll("/", "-")}.txt`), "work\n")
  await sh(["git", "-C", wtPath, "add", "."])
  await sh(["git", "-C", wtPath, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", branch])
  await sh(["git", "-C", repo.local, "merge", "--no-ff", "-m", `merge ${branch}`, branch])
  return wtPath
}

function prEntry(branch: string, state: string, mergedAt: string | null): unknown {
  return { headRefName: branch, state, mergedAt }
}

/** gh-фиксчер принимает только JSON-строку — как реальный stdout gh. */
const setGh = (entries: unknown[]): void => {
  ghResponse = JSON.stringify(entries)
}

const gcArgs = (repo: TestRepo, extra?: Partial<GcArgs>): GcArgs => ({
  local: repo.local,
  roots: [repo.wtRoot, repo.legacyRoot],
  base: "master",
  protectedBranches: new Set(),
  protectedDirs: new Set(),
  nowMs: Date.now(),
  graceMs: GRACE_MS,
  ...extra,
})

const oldMergeIso = (): string => new Date(Date.now() - 2 * GRACE_MS).toISOString()

beforeAll(() => {
  ghResponse = null
  ghShouldFail = false
})

afterAll(() => {
  for (const root of roots) rmSync(root, { recursive: true, force: true })
})

describe("registeredWorktrees", () => {
  test("пары path/branch, detached без ветки — мимо", () => {
    const porcelain = [
      "worktree /repo",
      "HEAD abc",
      "branch refs/heads/master",
      "",
      "worktree /repo/wt/a",
      "HEAD def",
      "branch refs/heads/feat/a",
      "",
      "worktree /repo/wt/detached",
      "HEAD 123",
      "detached",
      "",
    ].join("\n")
    expect(registeredWorktrees(porcelain)).toEqual([
      { path: "/repo", branch: "master" },
      { path: "/repo/wt/a", branch: "feat/a" },
    ])
  })
})

describe("gcLegacyWorktrees — реальный git", () => {
  test("merged+clean: ворктри снесён, ветка удалена, пустой корень убран", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-a", "wt")
    setGh([prEntry("feat-a", "MERGED", oldMergeIso())])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([wtPath])
    expect(report.branchesDeleted).toEqual(["feat-a"])
    expect(report.failed).toEqual([])
    expect(existsSync(wtPath)).toBe(false)
    expect(await sh(["git", "-C", repo.local, "branch", "--list", "feat-a"])).toBe("")
    expect(existsSync(repo.wtRoot)).toBe(false)
  })

  test("грязный ворктри пропускается — каталог и ветка на месте", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-dirty", "wt")
    writeFileSync(join(wtPath, "draft.txt"), "незакоммиченное\n")
    setGh([prEntry("feat-dirty", "MERGED", oldMergeIso())])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([{ worktree: wtPath, branch: "feat-dirty", reason: "незакоммиченные правки" }])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("открытый PR — пропуск, даже если ветка уже смержена", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-open", "wt")
    setGh([prEntry("feat-open", "OPEN", null)])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([{ worktree: wtPath, branch: "feat-open", reason: "открытый PR" }])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("closed-unmerged (не предок base) — пропуск как спорный", async () => {
    const repo = await makeRepo()
    const wtPath = join(repo.legacyRoot, "rejected")
    await sh(["git", "-C", repo.local, "worktree", "add", wtPath, "-b", "feat-rejected"])
    writeFileSync(join(wtPath, "rejected.txt"), "work\n")
    await sh(["git", "-C", wtPath, "add", "."])
    await sh(["git", "-C", wtPath, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "rejected"])
    setGh([prEntry("feat-rejected", "CLOSED", null)])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([
      { worktree: wtPath, branch: "feat-rejected", reason: "ветка не смержена в master" },
    ])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("squash/rebase-мердж (MERGED в каталоге, не предок) — скип с точной причиной", async () => {
    const repo = await makeRepo()
    const wtPath = join(repo.wtRoot, "squashed")
    await sh(["git", "-C", repo.local, "worktree", "add", wtPath, "-b", "feat-squashed"])
    writeFileSync(join(wtPath, "squashed.txt"), "work\n")
    await sh(["git", "-C", wtPath, "add", "."])
    await sh(["git", "-C", wtPath, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "squashed work"])
    // squash-мердж в master (содержимое там, коммитов ветки нет) — имитация
    // обычным коммитом в master: ветка остаётся не-предком
    writeFileSync(join(repo.local, "squash-landed.txt"), "landed via squash\n")
    await sh(["git", "-C", repo.local, "add", "."])
    await sh(["git", "-C", repo.local, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "feat-squashed (squash)"])
    setGh([prEntry("feat-squashed", "MERGED", oldMergeIso())])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([
      {
        worktree: wtPath,
        branch: "feat-squashed",
        reason: "ветка не предок master при MERGED-PR (squash/rebase-мердж или коммиты поверх мержа)",
      },
    ])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("грейс после мерджа: свежий MERGED — пропуск, по истечении — снос", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-grace", "wt")
    setGh([prEntry("feat-grace", "MERGED", new Date(Date.now() - HOUR_MS).toISOString())])

    const first = await gcLegacyWorktrees(deps, gcArgs(repo))
    expect(first.removed).toEqual([])
    expect(first.skipped).toEqual([{ worktree: wtPath, branch: "feat-grace", reason: "грейс после мерджа" }])
    expect(existsSync(wtPath)).toBe(true)

    const second = await gcLegacyWorktrees(deps, gcArgs(repo, { nowMs: Date.now() + GRACE_MS }))
    expect(second.removed).toEqual([wtPath])
    expect(existsSync(wtPath)).toBe(false)
  })

  test("коммиты поверх мержа — пропуск: ветка перестала быть предком base", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-ahead", "wt")
    writeFileSync(join(wtPath, "extra.txt"), "поверх мержа\n")
    await sh(["git", "-C", wtPath, "add", "."])
    await sh(["git", "-C", wtPath, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "extra"])
    setGh([prEntry("feat-ahead", "MERGED", oldMergeIso())])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([
      {
        worktree: wtPath,
        branch: "feat-ahead",
        reason: "ветка не предок master при MERGED-PR (squash/rebase-мердж или коммиты поверх мержа)",
      },
    ])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("активная задача защищается веткой и каталогом", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-active", "wt")
    setGh([prEntry("feat-active", "MERGED", oldMergeIso())])

    const byBranch = await gcLegacyWorktrees(
      deps,
      gcArgs(repo, { protectedBranches: new Set(["feat-active"]) }),
    )
    expect(byBranch.skipped).toEqual([{ worktree: wtPath, branch: "feat-active", reason: "активная задача" }])

    const byDir = await gcLegacyWorktrees(deps, gcArgs(repo, { protectedDirs: new Set([wtPath]) }))
    expect(byDir.skipped).toEqual([{ worktree: wtPath, branch: "feat-active", reason: "активная задача" }])
    expect(existsSync(wtPath)).toBe(true)
  })

  test("сбой gh останавливает проход без единого решения", async () => {
    const repo = await makeRepo()
    await mergedBranchWorktree(repo, "feat-ghfail", "wt")
    ghShouldFail = true

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))
    ghShouldFail = false

    expect(report.abortedReason).toBe("каталог PR недоступен")
    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([])
  })

  test("предок base без PR — снос по git-истине", async () => {
    const repo = await makeRepo()
    const wtPath = await mergedBranchWorktree(repo, "feat-nopr", "wt")
    setGh([])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([wtPath])
    expect(existsSync(wtPath)).toBe(false)
  })

  test("ворктри вне легаси-корней — не кандидат, не трогается", async () => {
    const repo = await makeRepo()
    const outsidePath = join(repo.root, "outside-wt")
    await sh(["git", "-C", repo.local, "worktree", "add", outsidePath, "-b", "feat-outside"])
    await sh(["git", "-C", repo.local, "merge", "--no-ff", "-m", "merge outside", "feat-outside"])
    setGh([prEntry("feat-outside", "MERGED", oldMergeIso())])

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.skipped).toEqual([])
    expect(existsSync(outsidePath)).toBe(true)
  })

  test("нет легаси-корней — мгновенный no-op без gh", async () => {
    const repo = await makeRepo()
    ghResponse = null

    const report = await gcLegacyWorktrees(deps, gcArgs(repo))

    expect(report.removed).toEqual([])
    expect(report.abortedReason).toBeUndefined()
  })
})
