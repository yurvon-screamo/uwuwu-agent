// lib/screen-review-render.ts — рендер markdown-отчёта screen_review.
// Вынесен из ядра (qlty smells file-complexity, ревью кода 13.11: ядро
// совмещает типы/чек-лист/парсинг/порог/рендер — рендер первый кандидат
// на вынос). Только отрисовка: порог-семантика — problemProbability в core.
import {
  problemProbability,
  type DecisionAnswer,
  type ScreenVerdict,
} from "./screen-review-core"

export function scoreVerdictLabel(answer: DecisionAnswer): string | null {
  if (answer.type !== "score" || !answer.legend) return null
  const lo = Math.floor(answer.score)
  const hi = Math.ceil(answer.score)
  const loLabel = answer.legend[String(lo)]
  const hiLabel = answer.legend[String(hi)]
  if (lo === hi || !loLabel) return loLabel ?? null
  return hiLabel ? `${loLabel} → ${hiLabel}` : loLabel
}

export function renderVerdictSection(verdict: ScreenVerdict, state: string, threshold: number): string {
  const lines: string[] = []
  lines.push(`### Screenshot: ${verdict.imageName}`)
  lines.push("")
  lines.push(`Path: \`${verdict.imagePath}\` · state: ${state} · input_tokens: ${verdict.inputTokens}`)
  lines.push("")
  lines.push("| Question | Type | Verdict | P (problem) | Flag |")
  lines.push("| --- | --- | --- | --- | --- |")
  const flags: Array<{ id: string; p: number }> = []
  for (const [id, answer] of Object.entries(verdict.answers)) {
    const p = problemProbability(answer)
    const flagged = p !== null && p > threshold
    if (flagged && p !== null) flags.push({ id, p })
    lines.push(`| ${id} | ${answer.type} | ${verdictCell(answer)} | ${pCell(p)} | ${flagged ? "⚠️" : "—"} |`)
  }
  lines.push("")
  if (flags.length > 0) {
    lines.push(`**Signals** (P > ${threshold}):`)
    for (const { id, p } of flags.sort((a, b) => b.p - a.p)) {
      lines.push(`- ${id} — P ${round(p)}`)
    }
  } else {
    lines.push(`**No signals** — no question exceeded the ${threshold} threshold.`)
  }
  return lines.join("\n")
}

function verdictCell(answer: DecisionAnswer): string {
  if (answer.type === "noul") return round(answer.noul)
  if (answer.type === "choice") {
    const conf = answer.confidence !== undefined ? ` · conf ${round(answer.confidence)}` : ""
    return `${answer.choice} (${round(answer.probabilities[answer.choice] ?? 0)})${conf}`
  }
  const label = scoreVerdictLabel(answer)
  return `${round(answer.score)}${label ? ` — ${label}` : ""}`
}

const pCell = (p: number | null): string => (p === null ? "—" : round(p))
const round = (n: number): string => String(Math.round(n * 100) / 100)

export function renderReport(sections: string[], meta: { model: string; checklistName: string }, threshold: number): string {
  const head = [
    `## screen_review (${meta.model}, checklist ${meta.checklistName}, threshold ${threshold})`,
    "",
  ]
  const tail = [
    "",
    "> The tool is a screener/aimer, not the final judge: the flags show where to look with your own eyes; the final judgment — the agent/human.",
  ]
  return [...head, ...sections, ...tail].join("\n")
}
