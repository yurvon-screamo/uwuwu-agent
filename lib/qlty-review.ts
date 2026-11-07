// lib/qlty-review.ts — оркестрация машинного ревьюера qlty_review (тул
// OpenCode, регистрация — plugins/qlty-review.ts; чистые функции —
// qlty-review-core.ts). Алгоритмический слой фабричного ревью по мотивам
// qltysh: qlty check (SARIF) + qlty smells --upstream по дифу/коммиту,
// delta-классификация против пробы merge-base во временном worktree
// (механика delta-vs-baseline из Lint-спеки developer.md). Ниша после
// CI-гейта (ADR-063): пре-пуш фаза девелопера и верификация ворктри
// ревьювером — незакоммиченный хвост и untracked CI не видит никогда.
// Без зависимости от uwuwu-cli: git+qlty в любом qlty-репо.

import { execFile } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { promisify } from "node:util"
import {
  applyFilters,
  classifyDelta,
  parseSarif,
  renderReport,
  type QltyFinding,
  type ReviewInput,
  type ReviewScope,
} from "./qlty-review-core"

const exec = promisify(execFile)

// qlty check exit-код ≠0 при находках (fail-level fmt) — это НЕ сбой:
// SARIF печатается в stdout, глотаем только пустые отказы.
async function runOut(cmd: string, args: string[], cwd: string): Promise<string> {
  try {
    const r = await exec(cmd, args, { cwd, maxBuffer: 64 * 1024 * 1024 })
    return r.stdout
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string; message?: string }
    if (err.stdout && err.stdout.trim() !== "") return err.stdout
    throw new Error(`${cmd} ${args.join(" ")} (cwd=${cwd}) — отказ: ${err.stderr?.trim() || err.message || "неизвестная ошибка"}`)
  }
}

export const realDeps: ReviewDeps = {
  git: (args, cwd) => runOut("git", args, cwd),
  qlty: (args, cwd) => runOut("qlty", args, cwd),
}

// ——— типы ———

export interface ReviewDeps {
  git: (args: string[], cwd: string) => Promise<string>
  qlty: (args: string[], cwd: string) => Promise<string>
}

export interface ReviewResult {
  markdown: string
  qltyVersion: string
  introduced: number
  baseline: number
  smells: number
}

export type { QltyFinding, ReviewInput, ReviewScope }

// ——— оркестрация ———

export async function runReview(input: ReviewInput, deps: ReviewDeps = realDeps): Promise<ReviewResult> {
  const dir = input.directory
  const upstream = input.upstream ?? "origin/master"

  const base = (await deps.git(["merge-base", upstream, "HEAD"], dir)).trim()
  if (base === "") throw new Error(`merge-base ${upstream}..HEAD пуст — репо не git или upstream не существует`)

  let paths: string[]
  const notes: string[] = []
  if (input.scope === "commit") {
    if (!input.commit) throw new Error("scope=commit требует commit (SHA/реф)")
    paths = (await deps.git(["diff-tree", "--no-commit-id", "--name-only", "-r", input.commit], dir))
      .split("\n")
      .filter(Boolean)
    notes.push(
      "scope=commit: прогон по рабочей копии — корректен, если файлы на диске в состоянии коммита (свежий HEAD в чистом ворктри).",
    )
  } else {
    const changed = (await deps.git(["diff", "--name-only", "--diff-filter=d", base], dir)).split("\n").filter(Boolean)
    const untracked = (await deps.git(["ls-files", "--others", "--exclude-standard"], dir)).split("\n").filter(Boolean)
    paths = [...new Set([...changed, ...untracked])]
  }

  const currentRaw = await deps.qlty(["check", "--sarif", "--no-fix", "--no-progress", ...paths], dir)
  const current = parseSarif(currentRaw, "qlty check")

  // Baseline-проба: временный worktree на merge-base, те же пути.
  // Новые файлы (нет на base) из baseline-пробы исключаются.
  const baseFiles = new Set(
    (await deps.git(["ls-tree", "-r", "--name-only", base], dir)).split("\n").filter(Boolean),
  )
  const basePaths = paths.filter((p) => baseFiles.has(p))

  const tmp = mkdtempSync(join(tmpdir(), "qlty-review-baseline-"))
  let baseline: QltyFinding[] = []
  try {
    await deps.git(["worktree", "add", "--detach", "--quiet", tmp, base], dir)
    try {
      if (basePaths.length > 0) {
        const baselineRaw = await deps.qlty(["check", "--sarif", "--no-fix", "--no-progress", ...basePaths], tmp)
        baseline = parseSarif(baselineRaw, "qlty check (baseline)").findings
      }
    } finally {
      // лучшая попытка: не маскируем исходную ошибку прогона
      try {
        await deps.git(["worktree", "remove", "--force", tmp], dir)
      } catch {
        rmSync(tmp, { recursive: true, force: true })
      }
    }
  } catch (e) {
    rmSync(tmp, { recursive: true, force: true })
    throw e
  }

  // Smells: нативно диффовый режим qlty
  const smellsRaw = await deps.qlty(["smells", "--sarif", "--upstream", base, "--quiet"], dir)
  const smells = parseSarif(smellsRaw, "qlty smells").findings

  const level = input.level ?? "low"
  const currentFiltered = applyFilters(current.findings, { paths: input.paths, level })
  const baselineFiltered = applyFilters(baseline, { paths: input.paths, level })
  const smellsFiltered = applyFilters(smells, { paths: input.paths, level })

  const { introduced, baselineMatched } = classifyDelta(currentFiltered, baselineFiltered)

  const markdown = renderReport({
    qltyVersion: current.qltyVersion,
    scope: input.scope,
    upstream,
    directory: dir,
    commit: input.commit,
    notes,
    introduced,
    baselineMatched,
    smells: smellsFiltered,
  })

  return {
    markdown,
    qltyVersion: current.qltyVersion,
    introduced: introduced.length,
    baseline: baselineMatched.length,
    smells: smellsFiltered.length,
  }
}
