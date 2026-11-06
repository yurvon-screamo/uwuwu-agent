// worktree-gc.ts — гигиена легаси-корней ворктри (wt/, .worktrees/ под
// master-чекаутом): снос по факту смержа PR. Кандидаты — зарегистрированные
// ворктри внутри корней; гарды обязательны (все, иначе skip+лог): активная
// задача, грязный статус, открытый PR, ветка не смержена в base, грейс
// после мерджа. Решений без данных нет: сбой gh останавливает весь проход,
// standalone при нечитаемом state — тоже (активные задачи не защищаются).
// git-операции через инъекцию run(): прод — фабричный sh() с таймаутом,
// тесты — реальный Bun.spawn на temp-репо, gh — фейковый фиксчер.
import { existsSync, rmdirSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { activeTaskGuards } from "./factory-lib"

/** Грейс-период после мерджа: сутки на осознание (reopen/cherry-pick/проверки). */
export const MERGE_GRACE_MS_DEFAULT = 24 * 60 * 60 * 1000

export interface GcDeps {
  /** git/gh-исполнитель: небивающийся процесс → throw, stdout → string. */
  run(cmd: string[], cwd?: string): Promise<string>
  log(message: string): void
}

export interface GcSkip {
  worktree: string
  branch: string
  reason: string
}

export interface GcReport {
  removed: string[]
  branchesDeleted: string[]
  skipped: GcSkip[]
  failed: GcSkip[]
  /** проход остановлен до решений (сбой porcelain/gh) — причина */
  abortedReason?: string
}

export interface GcArgs {
  /** главный чекаут репо (там .git и регистрация ворктри) */
  local: string
  /** легаси-корни для свипа (абсолютные пути) */
  roots: string[]
  /** ref, в который ветка должна быть смержена (прод — origin/master) */
  base: string
  /** ветки активных задач — не трогать никогда */
  protectedBranches: ReadonlySet<string>
  /** каталоги активных задач (worktree/envDir) — не трогать никогда */
  protectedDirs: ReadonlySet<string>
  nowMs: number
  /** грейс-период после мерджа (мс) */
  graceMs: number
}

export interface RegisteredWorktree {
  path: string
  branch: string
}

/** porcelain → пары path/branch. Detached (без строки branch) — мимо:
 *  без ветки нет PR и нет решения о сносе. */
export function registeredWorktrees(porcelain: string): RegisteredWorktree[] {
  const out: RegisteredWorktree[] = []
  let path = ""
  let branch = ""
  const flush = (): void => {
    if (path !== "" && branch !== "") out.push({ path, branch })
  }
  for (const line of porcelain.split("\n")) {
    if (line.startsWith("worktree ")) {
      flush()
      path = line.slice("worktree ".length)
      branch = ""
    } else if (line.startsWith("branch refs/heads/")) {
      branch = line.slice("branch refs/heads/".length)
    }
  }
  flush()
  return out
}

interface PrEntry {
  state: string
  mergedAt: string | null
}

/** Каталог PR одним вызовом gh: ветка → её PR-записи. null — gh не ответил:
 *  проход без данных решений не принимает. */
async function prCatalog(deps: GcDeps, local: string): Promise<Map<string, PrEntry[]> | null> {
  let raw: string
  try {
    raw = await deps.run(
      ["gh", "pr", "list", "--state", "all", "--limit", "500", "--json", "headRefName,state,mergedAt"],
      local,
    )
  } catch (e) {
    deps.log(`gc: каталог PR не получен (${errText(e)}) — проход остановлен, ничего не сношу`)
    return null
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch (e) {
    deps.log(`gc: каталог PR не JSON (${errText(e)}) — проход остановлен, ничего не сношу`)
    return null
  }
  const out = new Map<string, PrEntry[]>()
  if (!Array.isArray(parsed)) return out
  for (const item of parsed as Record<string, unknown>[]) {
    const head = item.headRefName
    if (typeof head !== "string" || head === "") continue
    const state = typeof item.state === "string" ? item.state : ""
    const mergedAt = typeof item.mergedAt === "string" ? item.mergedAt : null
    const list = out.get(head) ?? []
    list.push({ state, mergedAt })
    out.set(head, list)
  }
  return out
}

async function isAncestor(deps: GcDeps, local: string, branch: string, base: string): Promise<boolean> {
  try {
    await deps.run(["git", "-C", local, "merge-base", "--is-ancestor", branch, base])
    return true
  } catch {
    return false
  }
}

function isProtected(wt: RegisteredWorktree, args: GcArgs): boolean {
  if (args.protectedBranches.has(wt.branch)) return true
  for (const dir of args.protectedDirs) {
    if (wt.path === dir || wt.path.startsWith(`${dir}/`)) return true
  }
  return false
}

/** ms свежайшего мерджа ветки или null — MERGED-записей нет. */
function latestMergeAtMs(prs: PrEntry[]): number | null {
  let latest: number | null = null
  for (const pr of prs) {
    if (pr.state !== "MERGED" || pr.mergedAt === null) continue
    const ms = Date.parse(pr.mergedAt)
    if (Number.isNaN(ms)) continue
    if (latest === null || ms > latest) latest = ms
  }
  return latest
}

export async function gcLegacyWorktrees(deps: GcDeps, args: GcArgs): Promise<GcReport> {
  const report: GcReport = { removed: [], branchesDeleted: [], skipped: [], failed: [] }
  const roots = args.roots.filter((root) => existsSync(root))
  if (roots.length === 0) return report

  let porcelain: string
  try {
    porcelain = await deps.run(["git", "-C", args.local, "worktree", "list", "--porcelain"])
  } catch (e) {
    report.abortedReason = `worktree list не удался: ${errText(e)}`
    deps.log(`gc: ${report.abortedReason}`)
    return report
  }
  const candidates = registeredWorktrees(porcelain).filter((wt) =>
    roots.some((root) => wt.path === root || wt.path.startsWith(`${root}/`)),
  )
  if (candidates.length === 0) return report

  // fetch base — best-effort (упал → локальные рефы, консервативнее не станет:
  // несмерженное по старым рефам — skip, не снос)
  try {
    await deps.run(["git", "-C", args.local, "fetch", "origin", shortBaseRef(args.base)])
  } catch (e) {
    deps.log(`gc: fetch base не удался (${errText(e)}) — работаю на локальных рефах`)
  }

  const catalog = await prCatalog(deps, args.local)
  if (catalog === null) {
    report.abortedReason = "каталог PR недоступен"
    return report
  }

  for (const wt of candidates) {
    const skip = (reason: string): void => {
      report.skipped.push({ worktree: wt.path, branch: wt.branch, reason })
      deps.log(`gc: ${wt.path} (${wt.branch}) — пропуск: ${reason}`)
    }
    if (isProtected(wt, args)) {
      skip("активная задача")
      continue
    }
    let status = ""
    try {
      status = await deps.run(["git", "-C", wt.path, "status", "--porcelain"])
    } catch {
      // каталог срезан/недоступен — чистоту решает сам git worktree remove
    }
    if (status.trim() !== "") {
      skip("незакоммиченные правки")
      continue
    }
    const prs = catalog.get(wt.branch) ?? []
    if (prs.some((pr) => pr.state === "OPEN")) {
      skip("открытый PR")
      continue
    }
    const mergedInCatalog = prs.some((pr) => pr.state === "MERGED")
    if (!(await isAncestor(deps, args.local, wt.branch, args.base))) {
      // MERGED в каталоге + не-предок: squash/rebase-мердж или коммиты поверх
      // мержа — без mergeCommit из gh причины не различить, честно называем обе
      skip(
        mergedInCatalog
          ? `ветка не предок ${args.base} при MERGED-PR (squash/rebase-мердж или коммиты поверх мержа)`
          : `ветка не смержена в ${args.base}`,
      )
      continue
    }
    const mergedAtMs = latestMergeAtMs(prs)
    if (mergedAtMs !== null && args.nowMs - mergedAtMs < args.graceMs) {
      skip("грейс после мерджа")
      continue
    }
    try {
      await deps.run(["git", "-C", args.local, "worktree", "remove", wt.path])
      report.removed.push(wt.path)
      deps.log(`gc: снесён ${wt.path} (${wt.branch})`)
    } catch (e) {
      report.failed.push({ worktree: wt.path, branch: wt.branch, reason: errText(e) })
      deps.log(`gc: снос не удался ${wt.path} (${wt.branch}): ${errText(e)}`)
      continue
    }
    try {
      await deps.run(["git", "-C", args.local, "branch", "-d", wt.branch])
      report.branchesDeleted.push(wt.branch)
    } catch {
      deps.log(`gc: ветка ${wt.branch} не удалена (несмерженная или занята) — остаётся`)
    }
  }

  try {
    await deps.run(["git", "-C", args.local, "worktree", "prune"])
  } catch (e) {
    deps.log(`gc: prune не удался (${errText(e)})`)
  }
  // опустевший легаси-корень убираем; не пустой — ENOTEMPTY, остаётся
  for (const root of roots) {
    try {
      rmdirSync(root)
    } catch {}
  }
  return report
}

function shortBaseRef(base: string): string {
  return base.startsWith("origin/") ? base.slice("origin/".length) : base
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

// ——— standalone: разовый снос легаси-хвостов uwuwu первым ходом механики
// (рестарт демона не нужен). Активные задачи защищаются записями state. ———

/** shell-исполнитель standalone-входа: таймаут как у фабричного sh(). */
async function sh(cmd: string[], cwd?: string): Promise<string> {
  const proc = Bun.spawn({ cmd, cwd, stdout: "pipe", stderr: "pipe" })
  const killer = setTimeout(() => proc.kill(), 60_000)
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  const code = await proc.exited
  clearTimeout(killer)
  if (code !== 0) throw new Error(`${cmd[0]} (${code}): ${(err.trim() || out.trim()).slice(0, 200)}`)
  return out.trim()
}

/** gard-наборы из state.json standalone-прогона. null — state нечитаем:
 *  паритет с gh-абортом — решений без данных об активных задачах нет. */
async function standaloneGuards(stateFile: string): Promise<{ branches: Set<string>; dirs: Set<string> } | null> {
  try {
    return activeTaskGuards(JSON.parse(await Bun.file(stateFile).text()))
  } catch (e) {
    console.log(`state не прочитан (${errText(e)}) — abort: активные задачи не защищаются`)
    return null
  }
}

async function standaloneMain(): Promise<void> {
  const local = join(homedir(), "uwuwu-agent")
  const stateFile = join(homedir(), ".local/state/uwuwu-factory/state.json")
  const deps: GcDeps = { run: sh, log: (m) => console.log(m) }
  const guards = await standaloneGuards(stateFile)
  if (guards === null) process.exit(1)
  const report = await gcLegacyWorktrees(deps, {
    local,
    roots: [join(local, "wt"), join(local, ".worktrees")],
    base: "origin/master",
    protectedBranches: guards.branches,
    protectedDirs: guards.dirs,
    nowMs: Date.now(),
    graceMs: MERGE_GRACE_MS_DEFAULT,
  })
  const summary =
    `снесено=${report.removed.length}, веток=${report.branchesDeleted.length}, ` +
    `пропущено=${report.skipped.length}, неудач=${report.failed.length}` +
    (report.abortedReason !== undefined ? `, прерван: ${report.abortedReason}` : "")
  console.log(`итог: ${summary}`)
  if (report.abortedReason !== undefined) process.exit(1)
}

if (import.meta.main) {
  void standaloneMain()
}
