// factory/autodone.ts — контуры автодона (задача 20261112160316): носитель
// PR, вердикты проб GitHub/Gitea, решение одного тика. Чистые функции —
// тестируемые; сайд-эффекты (gh/tea/HTTP-пробы, merge) — в factory.ts.

/** Носитель PR задачи: определяется по PR-ячейке (первоисточник), при пустой
 *  ячейке с непустой веткой — фоллбек по ветке (семейство ADR-086), носитель
 *  выбирается по shape репо (первый сегмент с точкой = Gitea-хост, иначе
 *  GitHub). Прочие URL (wiki-артефакт docs-only канона) и пустая ячейка без
 *  ветки — «без ПР». */
export type PrCarrier =
  | { kind: "github"; url: string }
  | { kind: "gitea"; url: string; host: string; index: string; slug: string }
  | { kind: "github-branch"; branch: string }
  | { kind: "gitea-branch"; branch: string; host: string; slug: string }
  | { kind: "none" }

const GITHUB_PR = /\/pull\/(\d+)/
const GITEA_PR = /\/pulls\/(\d+)/

/** Репо-строка конфига → Gitea (host, slug) или null (GitHub: owner/name). */
export function giteaRepoOf(repo: string): { host: string; slug: string } | null {
  const parts = repo.split("/")
  if (parts.length !== 3) return null
  const [host, owner, name] = parts
  return host.includes(".") ? { host, slug: `${owner}/${name}` } : null
}

export function prCarrierOf(prCell: string, branch: string, repo: string): PrCarrier {
  const url = prCell.trim()
  if (url !== "") {
    const gh = url.match(GITHUB_PR)
    if (gh) return { kind: "github", url }
    const gt = url.match(GITEA_PR)
    if (gt) {
      const parsed = new URL(url)
      const host = parsed.host
      const slug = parsed.pathname.split("/pulls/")[0]?.replace(/^\//, "") ?? ""
      return { kind: "gitea", url, host, index: gt[1] ?? "", slug }
    }
    return { kind: "none" }
  }
  const br = branch.trim()
  if (br !== "") {
    const gt = giteaRepoOf(repo)
    if (gt) return { kind: "gitea-branch", branch: br, host: gt.host, slug: gt.slug }
    return { kind: "github-branch", branch: br }
  }
  return { kind: "none" }
}

/** Нормализованный исход пробы PR: один словарь для обоих носителей. */
export type PrProbeState =
  | "merged"
  | "clean"
  | "conflict"
  | "checks-failed"
  | "checks-pending"
  | "unknown"

/** Вердикт gh pr view (state/mergeable/mergeStateStatus/statusCheckRollup).
 *  required-проверки в полёте дают BLOCKED с PENDING-rollup, упавшие —
 *  FAILURE; UNSTABLE = mergeable при не-обязательных упавших (мерж допустим). */
export function githubProbeVerdict(view: {
  state?: string
  mergeable?: string | boolean
  mergeStateStatus?: string
  statusCheckRollup?: { state?: string; conclusion?: string }[]
}): PrProbeState {
  if (view.state === "MERGED") return "merged"
  if (view.state !== "OPEN") return "unknown"
  if (view.mergeable === "CONFLICTING" || view.mergeStateStatus === "DIRTY") return "conflict"
  const rollup = view.statusCheckRollup ?? []
  const checks = rollup.filter((c) => (c.state ?? "") !== "NEUTRAL")
  const failed = checks.some(
    (c) => c.conclusion === "FAILURE" || c.conclusion === "CANCELLED" || c.state === "FAILURE",
  )
  if (failed) return "checks-failed"
  const pending = checks.some((c) => (c.state ?? "") !== "COMPLETED")
  if (pending) return "checks-pending"
  return "clean"
}

/** Вердикт Gitea API (pulls/{index} + commits/{sha}/status). Пустой combined
 *  status = CI не настроен — не блокер (реальность foilcap: статусы часто
 *  пустые). failure → red, pending/warning → ждать, success/"" → зелёный. */
export function giteaProbeVerdict(
  pr: { state?: string; merged?: boolean; mergeable?: boolean },
  ciState: string,
): PrProbeState {
  if (pr.merged === true) return "merged"
  if ((pr.state ?? "") !== "open") return "unknown"
  if (pr.mergeable === false) return "conflict"
  if (ciState === "failure") return "checks-failed"
  if (ciState === "pending" || ciState === "warning") return "checks-pending"
  return "clean"
}

/** Решение одного тика автодона по пробе. Ждём зелёный (pending) до таймаута;
 *  сбой gh/Gitea (unknown) — streak до предела; дальше Needs attention —
 *  сбой инфраструктуры не наказывает задачу (паритет PR_PROBE_UNKNOWN_MAX). */
export type AutodoneAction =
  | { t: "merge" }
  | { t: "bounce"; reason: string }
  | { t: "done"; reason: string }
  | { t: "wait" }
  | { t: "escalate"; reason: string }

export function autodoneAction(
  probe: PrProbeState,
  opts: { pendingMs: number; timeoutMs: number; unknownStreak: number; unknownMax: number },
): AutodoneAction {
  switch (probe) {
    case "merged":
      return { t: "done", reason: "PR уже смержен (проба)" }
    case "clean":
      return { t: "merge" }
    case "conflict":
      return { t: "bounce", reason: "merge-конфликт PR" }
    case "checks-failed":
      return { t: "bounce", reason: "красный CI PR" }
    case "checks-pending":
      return opts.pendingMs >= opts.timeoutMs
        ? { t: "escalate", reason: `CI не завершился за ${Math.round(opts.timeoutMs / 60_000)} мин` }
        : { t: "wait" }
    case "unknown":
      return opts.unknownStreak + 1 >= opts.unknownMax
        ? { t: "escalate", reason: `проба PR не удалась ${opts.unknownStreak + 1} тиков подряд` }
        : { t: "wait" }
  }
}
