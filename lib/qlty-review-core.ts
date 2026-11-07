// lib/qlty-review-core.ts — чистые функции ядра машинного ревьюера: SARIF-
// парсинг с контрактом полей (анти-дрейф), glob/level-фильтры, delta-
// классификация «введено веткой vs pre-existing» (механика delta-vs-baseline
// из Lint-спеки developer.md), рендер markdown-отчёта. Без git/qlty-сайд-
// эффектов — тестируется напрямую (qlty-review.test.ts), оркестрация —
// qlty-review.ts.

// ——— типы ———

export interface QltyFinding {
  /** "ruff:F401" — атрибуция plugin:rule как у qltysh */
  ruleId: string
  /** SARIF-уровень: note | warning | error (грубее qlty- severity) */
  level: string
  path: string
  line: number
  message: string
}

export type ReviewScope = "diff" | "commit"
export type ReviewLevel = "note" | "low" | "medium" | "high"

export interface ReviewInput {
  scope: ReviewScope
  /** репо для прогона (cwd тула — сервер опенкода, не сессия агента) */
  directory: string
  /** дефолт origin/master */
  upstream?: string
  /** SHA/реф для scope=commit */
  commit?: string
  /** глоб-фильтр путей (*, **, ?) */
  paths?: string[]
  /** порог: note — показывать всё; ниже — SARIF-note отсекается. Дефолт low */
  level?: ReviewLevel
}

// ——— SARIF-контракт (анти-дрейф): нехватка любого поля = громкая ошибка ———

export const SARIF_CONTRACT_FIELDS = [
  "runs[0].tool.driver.semanticVersion",
  "runs[0].results[].ruleId",
  "runs[0].results[].level",
  "runs[0].results[].message.text",
  "runs[0].results[].locations[0].physicalLocation.artifactLocation.uri",
  "runs[0].results[].locations[0].physicalLocation.region.startLine",
] as const

const contractFail = (label: string, what: string): Error =>
  new Error(
    `${label}: контракт SARIF нарушен — ${what}. Ожидаемые поля: ${SARIF_CONTRACT_FIELDS.join(", ")}. qlty обновился и сменил формат?`,
  )

interface SarifDocShape {
  runs?: Array<{
    tool?: { driver?: { semanticVersion?: unknown } }
    results?: Array<{
      ruleId?: unknown
      level?: unknown
      message?: { text?: unknown }
      locations?: Array<{
        physicalLocation?: {
          artifactLocation?: { uri?: unknown }
          region?: { startLine?: unknown }
        }
      }>
    }>
  }>
}

export function parseSarif(raw: string, label: string): { findings: QltyFinding[]; qltyVersion: string } {
  let doc: SarifDocShape
  try {
    doc = JSON.parse(raw) as SarifDocShape
  } catch {
    throw new Error(`${label}: вывод не SARIF/JSON (qlty установлен и рабочий?)`)
  }
  const run = doc.runs?.[0]
  if (!run) throw contractFail(label, "нет runs[0]")
  const version = run.tool?.driver?.semanticVersion
  if (typeof version !== "string" || version === "") throw contractFail(label, "нет tool.driver.semanticVersion")
  const results = run.results ?? []
  if (!Array.isArray(results)) throw contractFail(label, "results не массив")

  const findings: QltyFinding[] = []
  for (let i = 0; i < results.length; i++) {
    const r = results[i]
    const loc = r.locations?.[0]?.physicalLocation
    const ruleId = r.ruleId
    const level = r.level
    const message = r.message?.text
    const path = loc?.artifactLocation?.uri
    const line = loc?.region?.startLine
    if (typeof ruleId !== "string" || ruleId === "") throw contractFail(label, `results[${i}].ruleId`)
    if (typeof level !== "string" || level === "") throw contractFail(label, `results[${i}].level`)
    if (typeof message !== "string") throw contractFail(label, `results[${i}].message.text`)
    if (typeof path !== "string" || path === "") throw contractFail(label, `results[${i}].locations[0].physicalLocation.artifactLocation.uri`)
    if (typeof line !== "number" || !Number.isFinite(line)) throw contractFail(label, `results[${i}].locations[0].physicalLocation.region.startLine`)
    findings.push({ ruleId, level, path, line, message })
  }
  return { findings, qltyVersion: version }
}

// ——— фильтры ———

// SARIF-уровни грубее qlty severity: error — верхний ранг (не отсекается
// никогда при пороге ≤ high), warning — низкий (проходит при low/note),
// note — отсекается любым порогом выше note.
const LEVEL_RANK: Record<string, number> = {
  note: 0,
  low: 1,
  medium: 2,
  high: 3,
  warning: 1,
  error: 3,
}

function globToRegExp(glob: string): RegExp {
  let re = ""
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]
    if (c === "*") {
      if (glob[i + 1] === "*") {
        re += ".*"
        i++
      } else {
        re += "[^/]*"
      }
    } else if (c === "?") {
      re += "[^/]"
    } else {
      re += c.replace(/[.+^${}()|[\]\\]/g, "\\$&")
    }
  }
  return new RegExp(`^${re}$`)
}

export function applyFilters(
  findings: QltyFinding[],
  opts: { paths?: string[]; level?: ReviewLevel },
): QltyFinding[] {
  const threshold = opts.level === "note" ? 0 : LEVEL_RANK[opts.level ?? "low"] ?? 1
  const globs = (opts.paths ?? []).map(globToRegExp)
  return findings.filter((f) => {
    if ((LEVEL_RANK[f.level] ?? 1) < threshold) return false
    if (globs.length > 0 && !globs.some((g) => g.test(f.path))) return false
    return true
  })
}

// ——— delta-классификация ———

// Ключ без line: строки сдвигаются правками выше, message стабильнее.
// Multiset-сверка: два одинаковых F401 на разных строках — два ключа.
const deltaKey = (f: QltyFinding): string => `${f.path}\u0000${f.ruleId}\u0000${f.message}`

export function classifyDelta(
  current: QltyFinding[],
  baseline: QltyFinding[],
): { introduced: QltyFinding[]; baselineMatched: QltyFinding[] } {
  const baseCounts = new Map<string, number>()
  for (const b of baseline) {
    const key = deltaKey(b)
    baseCounts.set(key, (baseCounts.get(key) ?? 0) + 1)
  }

  const introduced: QltyFinding[] = []
  const baselineMatched: QltyFinding[] = []
  for (const c of current) {
    const key = deltaKey(c)
    const left = baseCounts.get(key) ?? 0
    if (left > 0) {
      baseCounts.set(key, left - 1)
      baselineMatched.push(c)
    } else {
      introduced.push(c)
    }
  }
  return { introduced, baselineMatched }
}

// ——— рендер отчёта ———

const fmtFinding = (f: QltyFinding): string =>
  `- [${f.ruleId}] ${f.path}:${f.line} (${f.level}) — ${f.message}`

const fmtGroup = (title: string, findings: QltyFinding[]): string => {
  if (findings.length === 0) return ""
  return `**${title}**\n\n${findings.map(fmtFinding).join("\n")}\n`
}

export function renderReport(parts: {
  qltyVersion: string
  scope: ReviewScope
  upstream: string
  directory: string
  commit?: string
  notes: string[]
  introduced: QltyFinding[]
  baselineMatched: QltyFinding[]
  smells: QltyFinding[]
}): string {
  const head = `**Машинный ревью (qlty v${parts.qltyVersion})** — scope \`${parts.scope}\`${parts.commit ? ` (\`${parts.commit}\`)` : ""} · upstream \`${parts.upstream}\` · ${parts.directory}`
  const notes = parts.notes.length > 0 ? `\n${parts.notes.map((n) => `> ⚠ ${n}`).join("\n")}\n` : ""
  const body =
    fmtGroup("Введено веткой (delta)", parts.introduced) +
    fmtGroup("Baseline (pre-existing, не от этой ветки)", parts.baselineMatched) +
    fmtGroup("Smells (qlty smells, новые против upstream)", parts.smells)
  const total = parts.introduced.length + parts.smells.length
  const verdict =
    total === 0
      ? `Итог: ✅ новых машинных находок нет (baseline: ${parts.baselineMatched.length}).`
      : `Итог: ${parts.introduced.length} введено + ${parts.baselineMatched.length} baseline + ${parts.smells.length} smells.`
  return `${head}\n${notes}\n${body}\n${verdict}`
}
