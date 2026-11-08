// lib/qlty-review.ts — оркестрация машинного ревьюера qlty_review (тул
// OpenCode, регистрация — plugins/qlty-review.ts; чистые функции —
// qlty-review-core.ts). Механика — нативная строковая дельта qlty против
// upstream: `qlty check/smells --upstream <base>` репортит находки только
// в добавленных/изменённых строках — pre-existing baseline-долг файла не
// показывается (инцидент PR #158). Незакоммиченный хвост и untracked в
// дельту входят — ниша против CI-гейта (ADR-063: CI видит только
// запушенную дельту; в Actions checkout detached, tracking-глюк не
// активен — тут он нейтрализуется env, см. trackingNeutralizingEnv).
// Пробы механики 08.11 — задача 20261108060615-hjw3uqp, опыт
// /tools/«qlty CI Gate Analysis». Без зависимости от uwuwu-cli:
// git+qlty в любом qlty-репо.

import { execFile } from "node:child_process"
import { promisify } from "node:util"
import {
  applyFilters,
  parseSarif,
  renderReport,
  type QltyFinding,
  type ReviewInput,
  type ReviewScope,
} from "./qlty-review-core"

const exec = promisify(execFile)

// qlty check exit-код ≠0 при находках (fail-level fmt) — это НЕ сбой:
// SARIF печатается в stdout, глотаем только пустые отказы.
async function runOut(
  cmd: string,
  args: string[],
  cwd: string,
  env: Record<string, string> = {},
): Promise<string> {
  try {
    const r = await exec(cmd, args, { cwd, maxBuffer: 64 * 1024 * 1024, env: { ...process.env, ...env } })
    return r.stdout
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string; message?: string }
    if (err.stdout && err.stdout.trim() !== "") return err.stdout
    throw new Error(`${cmd} ${args.join(" ")} (cwd=${cwd}) — отказ: ${err.stderr?.trim() || err.message || "неизвестная ошибка"}`)
  }
}

export const realDeps: ReviewDeps = {
  git: (args, cwd) => runOut("git", args, cwd),
  qlty: (args, cwd, env) => runOut("qlty", args, cwd, env),
}

// ——— типы ———

export interface ReviewDeps {
  git: (args: string[], cwd: string) => Promise<string>
  qlty: (args: string[], cwd: string, env?: Record<string, string>) => Promise<string>
}

export interface ReviewResult {
  markdown: string
  qltyVersion: string
  delta: number
  smells: number
}

export type { QltyFinding, ReviewInput, ReviewScope }

// qlty 0.661.0: при branch-tracking текущей ветки флаг --upstream
// игнорируется — базой дельты становится tracking-реф, и закоммиченная
// дельта пост-пуш ворктри невидима (поймано ревью PR #179). Пустой
// branch.<name>.merge через GIT_CONFIG_* нейтрализует tracking для
// git-вызовов процесса qlty — --upstream уважается (пробы 08.11). В
// detached HEAD ветки нет — env не нужен (в т.ч. CI checkout).
async function trackingNeutralizingEnv(
  git: ReviewDeps["git"],
  dir: string,
): Promise<Record<string, string>> {
  const branch = (await git(["rev-parse", "--abbrev-ref", "HEAD"], dir)).trim()
  if (branch === "" || branch === "HEAD") return {}
  return {
    GIT_CONFIG_COUNT: "1",
    GIT_CONFIG_KEY_0: `branch.${branch}.merge`,
    GIT_CONFIG_VALUE_0: "",
  }
}

// ——— оркестрация ———

export async function runReview(input: ReviewInput, deps: ReviewDeps = realDeps): Promise<ReviewResult> {
  const dir = input.directory
  const upstream = input.upstream ?? "origin/master"

  // База дельты: merge-base для diff (ветка + хвост + untracked против точки
  // форка), родитель коммита для commit. qlty сравнивает рабочую копию с
  // базой построчно, сам определяя изменённые пути.
  let base: string
  const notes: string[] = []
  if (input.scope === "commit") {
    if (!input.commit) throw new Error("scope=commit требует commit (SHA/реф)")
    base = (await deps.git(["rev-parse", `${input.commit}^`], dir)).trim()
    if (base === "") throw new Error(`у ${input.commit} нет родителя — корневой коммит не имеет дельты`)
    notes.push(
      "scope=commit: прогон по рабочей копии — корректен, если файлы на диске в состоянии коммита (свежий HEAD в чистом ворктри).",
    )
  } else {
    base = (await deps.git(["merge-base", upstream, "HEAD"], dir)).trim()
    if (base === "") throw new Error(`merge-base ${upstream}..HEAD пуст — репо не git или upstream не существует`)
  }

  const qltyEnv = await trackingNeutralizingEnv(deps.git, dir)

  const deltaRaw = await deps.qlty(
    ["check", "--sarif", "--no-fix", "--no-progress", "--upstream", base],
    dir,
    qltyEnv,
  )
  const delta = parseSarif(deltaRaw, "qlty check (delta)")

  const smellsRaw = await deps.qlty(["smells", "--sarif", "--upstream", base, "--quiet"], dir, qltyEnv)
  const smells = parseSarif(smellsRaw, "qlty smells").findings

  const level = input.level ?? "low"
  const deltaFiltered = applyFilters(delta.findings, { paths: input.paths, level })
  const smellsFiltered = applyFilters(smells, { paths: input.paths, level })

  const markdown = renderReport({
    qltyVersion: delta.qltyVersion,
    scope: input.scope,
    upstream,
    directory: dir,
    commit: input.commit,
    notes,
    delta: deltaFiltered,
    smells: smellsFiltered,
  })

  return {
    markdown,
    qltyVersion: delta.qltyVersion,
    delta: deltaFiltered.length,
    smells: smellsFiltered.length,
  }
}
