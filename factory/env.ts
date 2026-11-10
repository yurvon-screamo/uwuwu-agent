// env.ts — окружение задачи: <root>/<rowId>/{repo,scratch,logs}.
// Жизненный цикл ведёт фабрика: ensureEnv на спавне (идемпотентен —
// переживает потерю state.json), cleanupEnv на Done, sweepEnvs на старте
// демона. git-операции через инъекцию run(): прод — фабричный sh() с
// таймаутом, тесты — реальный Bun.spawn на temp-репо.
import { existsSync, mkdirSync, readdirSync, rmSync, renameSync, statSync } from "node:fs"
import { join } from "node:path"

export interface EnvDeps {
  /** git/FS-shell исполнитель: небивающийся процесс → throw, stdout → string. */
  run(cmd: string[], cwd?: string): Promise<string>
  log(message: string): void
}

export interface EnvPaths {
  envDir: string
  repoDir: string
  scratchDir: string
  logsDir: string
  branch: string
}

/** row-id → безопасный фрагмент имени каталога/ветки. Полный id (не префикс):
 *  префикс = секунда создания, две задачи одной секунды коллидировали бы
 *  в одной ветке. */
export function sanitizeRowId(rowId: string): string {
  const safe = rowId.replace(/^-+/, "").replace(/[^a-zA-Z0-9_-]/g, "")
  return safe === "" ? "task" : safe
}

export function envPaths(root: string, rowId: string): EnvPaths {
  const safe = sanitizeRowId(rowId)
  const envDir = join(root, safe)
  return {
    envDir,
    repoDir: join(envDir, "repo"),
    scratchDir: join(envDir, "scratch"),
    logsDir: join(envDir, "logs"),
    branch: `work/task-${safe}`,
  }
}

const ATTIC_DIR = ".attic"
/** Тяжёлые игнорируемые каталоги, вырезаемые из attic-копии (porcelain их
 *  не видит, а место они держат основное). Ищутся на двух уровнях repo. */
const HEAVY_DIR_NAMES = ["target", "node_modules"]

export interface EnsureEnvArgs {
  root: string
  rowId: string
  /** главный чекаут репо (там живёт .git и refs) */
  local: string
  /** база ворктри; прод — baseOf(cfg) проекта, тесты — локальная ветка */
  base: string
}

/** Идемпотентное создание окружения: живой зарегистрированный ворктри →
 *  reuse; ветка уже есть (ретрай после частичного фейла) → attach к ней;
 *  fetch base — best-effort (упал → локальные рефы, спавн не роняем). */
export async function ensureEnv(deps: EnvDeps, args: EnsureEnvArgs): Promise<EnvPaths> {
  const paths = envPaths(args.root, args.rowId)
  if (existsSync(paths.repoDir)) {
    const registered = await worktreeListContains(deps, args.local, paths.repoDir)
    if (registered) return paths
    // каталог без регистрации в .git — обломок прерванного прогона (линка
    // .git нет либо регистрация срезана prune): содержимое недостижимо —
    // пересоздаём с громким логом
    deps.log(`env ${sanitizeRowId(args.rowId)}: ворктри без регистрации — сношу обломок и пересоздаю`)
    rmSync(paths.repoDir, { recursive: true, force: true })
  }
  try {
    await deps.run(["git", "-C", args.local, "fetch", "origin", shortRef(args.base)])
  } catch (e) {
    deps.log(`env ${sanitizeRowId(args.rowId)}: fetch не удался (${errText(e)}) — работаю на локальных рефах`)
  }
  try {
    await deps.run(["git", "-C", args.local, "worktree", "add", "-b", paths.branch, paths.repoDir, args.base])
  } catch (addErr) {
    // ветка могла остаться от ретрая (частичный фейл между -b и записью state)
    deps.log(`env ${sanitizeRowId(args.rowId)}: worktree add -b не удался (${errText(addErr)}) — пробую attach к существующей ветке`)
    await deps.run(["git", "-C", args.local, "worktree", "add", paths.repoDir, paths.branch])
  }
  mkdirSync(paths.envDir, { recursive: true })
  mkdirSync(paths.scratchDir, { recursive: true })
  mkdirSync(paths.logsDir, { recursive: true })
  return paths
}

export type CleanupOutcome = "removed" | "atticked" | "failed"

export interface CleanupEnvArgs {
  root: string
  rowId: string
  /** главный чекаут (там .git и регистрация ворктри) */
  local: string
  /** фактический envDir из state (если запись его несёт) */
  envDir?: string
  branch?: string
}

/** Снос окружения на Done. Чистое → rm; грязное → attic (без target/
 *  node_modules) + пометка atticked (коммент владельцу — дело вызывающего).
 *  Порядок фиксирован: rm/attic → worktree prune → branch -d (до prune ветка
 *  числится за ворктри и безопасный -d падает всегда). */
export async function cleanupEnv(deps: EnvDeps, args: CleanupEnvArgs): Promise<CleanupOutcome> {
  const paths = envPaths(args.root, args.rowId)
  const envDir = args.envDir !== undefined && args.envDir !== "" ? args.envDir : paths.envDir
  const repoDir = envDir === paths.envDir ? paths.repoDir : join(envDir, "repo")
  let outcome: CleanupOutcome = "removed"
  try {
    if (existsSync(repoDir)) {
      const status = await deps.run(["git", "-C", repoDir, "status", "--porcelain"])
      if (status.trim() === "") {
        rmSync(envDir, { recursive: true, force: true })
      } else {
        const atticDir = join(args.root, ATTIC_DIR, `${sanitizeRowId(args.rowId)}-${atticStamp()}`)
        mkdirSync(join(args.root, ATTIC_DIR), { recursive: true })
        renameSync(envDir, atticDir)
        stripHeavyDirs(join(atticDir, "repo"))
        outcome = "atticked"
        deps.log(`env ${sanitizeRowId(args.rowId)}: незакоммиченное — attic ${atticDir}`)
      }
    } else {
      rmSync(envDir, { recursive: true, force: true })
    }
  } catch (e) {
    deps.log(`env ${sanitizeRowId(args.rowId)}: снос не удался (${errText(e)}) — каталог оставлен на месте`)
    return "failed"
  }
  try {
    await deps.run(["git", "-C", args.local, "worktree", "prune"])
  } catch (e) {
    deps.log(`env ${sanitizeRowId(args.rowId)}: prune не удался (${errText(e)})`)
  }
  if (args.branch !== undefined && args.branch !== "") {
    try {
      await deps.run(["git", "-C", args.local, "branch", "-d", args.branch])
    } catch {
      deps.log(`env ${sanitizeRowId(args.rowId)}: ветка ${args.branch} не удалена (несмерженная или чужая) — остаётся`)
    }
  }
  return outcome
}

export interface SweepEnvsArgs {
  root: string
  /** живые задачи state — их env не трогаем никогда */
  activeRowIds: Set<string>
  nowMs: number
  maxAgeMs: number
}

export interface SweepEnvsResult {
  sweptEnvs: string[]
  sweptAttic: string[]
}

/** Старт-свип: env-каталоги без записи в state и attic старше maxAge —
 *  снос. Реестр самочистящийся: содержимое root ≈ активные задачи.
 *  Гонка с чисткой (каталог исчез между readdir и stat) — пропуск записи,
 *  не падение старта демона. */
export function sweepEnvs(deps: EnvDeps, args: SweepEnvsArgs): SweepEnvsResult {
  const result: SweepEnvsResult = { sweptEnvs: [], sweptAttic: [] }
  if (!existsSync(args.root)) return result
  for (const entry of readdirSync(args.root)) {
    const full = join(args.root, entry)
    try {
      if (!statSync(full).isDirectory()) continue
      if (entry === ATTIC_DIR) {
        result.sweptAttic.push(...sweepAttic(deps, full, args.nowMs, args.maxAgeMs))
        continue
      }
      if (args.activeRowIds.has(entry)) continue
      if (args.nowMs - statSync(full).mtimeMs <= args.maxAgeMs) continue
      rmSync(full, { recursive: true, force: true })
      result.sweptEnvs.push(entry)
      deps.log(`env ${entry}: сирота (нет в state, старше ${Math.round(args.maxAgeMs / 86_400_000)} суток) — снесена`)
    } catch (e) {
      deps.log(`свип: ${entry} — пропуск (${errText(e)})`)
    }
  }
  return result
}

function sweepAttic(deps: EnvDeps, atticDir: string, nowMs: number, maxAgeMs: number): string[] {
  const swept: string[] = []
  for (const entry of readdirSync(atticDir)) {
    const full = join(atticDir, entry)
    try {
      if (nowMs - statSync(full).mtimeMs <= maxAgeMs) continue
      rmSync(full, { recursive: true, force: true })
      swept.push(entry)
      deps.log(`env attic/${entry}: старше ${Math.round(maxAgeMs / 86_400_000)} суток — снесена`)
    } catch (e) {
      deps.log(`свип attic: ${entry} — пропуск (${errText(e)})`)
    }
  }
  return swept
}

async function worktreeListContains(deps: EnvDeps, local: string, repoDir: string): Promise<boolean> {
  try {
    const porcelain = await deps.run(["git", "-C", local, "worktree", "list", "--porcelain"])
    return porcelain.split("\n").some((line) => line === `worktree ${repoDir}`)
  } catch (e) {
    deps.log(`env: worktree list не удался (${errText(e)})`)
    return false
  }
}

function stripHeavyDirs(repoDir: string): void {
  if (!existsSync(repoDir)) return
  for (const name of HEAVY_DIR_NAMES) rmSync(join(repoDir, name), { recursive: true, force: true })
  for (const entry of readdirSync(repoDir)) {
    const full = join(repoDir, entry)
    if (!statSync(full).isDirectory()) continue
    for (const name of HEAVY_DIR_NAMES) rmSync(join(full, name), { recursive: true, force: true })
  }
}

function atticStamp(): string {
  return new Date().toISOString().replace(/[-:T]/g, "").slice(0, 14)
}

function shortRef(base: string): string {
  return base.startsWith("origin/") ? base.slice("origin/".length) : base
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
