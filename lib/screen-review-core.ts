// lib/screen-review-core.ts — чистые функции ядра screen_review (тул ревью
// скринов над decision-моделью d1-omni через ollama /v1/systemone): встроенный
// чек-лист ui-slop, сборка запроса, парсинг ответа с контрактом полей
// (анти-дрейф), рендер markdown-отчёта. Без сети и ФС — тестируется напрямую
// (screen-review.test.ts), оркестрация — screen-review.ts. Форма — по
// прецеденту qlty_review (ADR-068). Живой контракт /v1/systemone зафиксирован
// пробами 13.11.2026 (questions — map, не массив; instructions/criteria).
//
// Семантика мульти-скрина (решение ревью плана 13.11): пер-скрин прогон —
// один вызов systemone на скрин, одинаковые вопросы; отчёт носит секцию на
// каждый скрин — вердикты атрибутированы к скрину нативно.

// ——— типы ———

export type QuestionType = "noul" | "choice" | "score"

/** Вопрос systemone: instructions — текст, criteria — опции choice
 * (label→описание) или уровни score (массив 2–10, от меньшего к большему). */
export interface ScreenQuestion {
  type: QuestionType
  instructions: string
  criteria?: Record<string, string> | string[]
}

/** Чек-лист — map id→вопрос (systemone принимает 1–64 вопросов). */
export type Checklist = Record<string, ScreenQuestion>

export interface SystemOneRequest {
  model: string
  state: string
  images: string[]
  questions: Checklist
}

export type SystemOneAnswer =
  | { type: "noul"; noul: number }
  | { type: "choice"; choice: string; probabilities: Record<string, number>; confidence?: number }
  | {
      type: "score"
      score: number
      legend?: Record<string, string>
      probabilities?: Record<string, number>
      confidence?: number
    }

export type SystemOneAnswers = Record<string, SystemOneAnswer>

export interface ScreenVerdict {
  /** путь к скрину, как передан на входе тула */
  imagePath: string
  /** base-имя файла для заголовка секции отчёта */
  imageName: string
  answers: SystemOneAnswers
  inputTokens: number
}

// ——— контракты (анти-дрейф): нехватка поля = громкая ошибка ———

export const ANSWER_CONTRACT_FIELDS = [
  "answers[<id>].type",
  "answers[<id>].noul — для type=noul",
  "answers[<id>].choice + answers[<id>].probabilities — для type=choice",
  "answers[<id>].score — для type=score",
] as const

export const DEFAULT_THRESHOLD = 0.6 // порог флага — из пилота pblpp97
export const DEFAULT_STATE = "Скриншот пользовательского интерфейса"
export const DEFAULT_CHECKLIST = "ui-slop"
export const MAX_QUESTIONS = 64

const contractFail = (label: string, what: string): Error =>
  new Error(
    `${label}: контракт ответа systemone нарушен — ${what}. Ожидаемые поля: ${ANSWER_CONTRACT_FIELDS.join(", ")}. ollama обновилась и сменила формат?`,
  )

// ——— встроенный чек-лист ui-slop (постановка задачи; источник истины
// паттернов — DESIGN.md канон origa, соответствие фиксирует док-заметка) ———

export const CHECKLISTS: Record<string, Checklist> = {
  [DEFAULT_CHECKLIST]: {
    double_frame: {
      type: "noul",
      instructions: "Есть ли двойная рамка у карточек или панелей (контур внутри контура, рамка внутри рамки)?",
    },
    card_in_card: {
      type: "noul",
      instructions: "Есть ли карточка внутри карточки (вложенная панель с рамкой или фоном внутри похожей внешней)?",
    },
    alignment: {
      type: "noul",
      instructions: "Нарушено ли выравнивание элементов (элемент выбивается из сетки или строки, неожиданный сдвиг)?",
    },
    overlap: {
      type: "noul",
      instructions: "Есть ли налезания элементов друг на друга или обрезка контента (текст или блок выходит за границы, перекрыт)?",
    },
    contrast: {
      type: "noul",
      instructions: "Есть ли проблемы контраста (текст плохо читается на фоне, слишком бледный, близкие цвета)?",
    },
    main_defect: {
      type: "choice",
      instructions: "Какой главный визуальный дефект экрана?",
      criteria: {
        none: "дефектов нет",
        double_frame: "двойные рамки",
        card_in_card: "карточка внутри карточки",
        alignment: "нарушенное выравнивание",
        overlap: "налезания или обрезка",
        contrast: "проблемы контраста",
        other: "другой дефект",
      },
    },
    severity: {
      type: "score",
      instructions: "Насколько сильны визуальные дефекты экрана?",
      criteria: ["чисто", "мелкие огрехи", "явные дефекты", "критично"],
    },
  },
}

// ——— валидация входа ———

export function resolveQuestions(input: {
  checklist?: string
  questions?: Checklist
}): Checklist {
  if (input.questions && input.checklist !== undefined) {
    throw new Error("screen_review: заданы и checklist, и questions — оставьте один источник вопросов")
  }
  const checklistName = input.checklist ?? DEFAULT_CHECKLIST
  const resolved = input.questions ?? CHECKLISTS[checklistName]
  if (!resolved) {
    const known = Object.keys(CHECKLISTS).join(", ")
    throw new Error(`screen_review: неизвестный чек-лист «${checklistName}» (встроенные: ${known}; свои вопросы — параметр questions)`)
  }
  const count = Object.keys(resolved).length
  if (count < 1 || count > MAX_QUESTIONS) {
    throw new Error(`screen_review: questions должен содержать 1–${MAX_QUESTIONS} вопросов, получено ${count}`)
  }
  return resolved
}

// ——— сборка запроса ———

export function buildRequest(model: string, state: string, imageBase64: string, questions: Checklist): SystemOneRequest {
  return { model, state, images: [imageBase64], questions }
}

// ——— парсинг ответа systemone ———

export function parseAnswers(raw: string, label: string): { answers: SystemOneAnswers; inputTokens: number } {
  let doc: {
    answers?: unknown
    usage?: { input_tokens?: unknown }
  }
  try {
    doc = JSON.parse(raw) as typeof doc
  } catch {
    throw contractFail(label, "тело ответа не JSON")
  }
  if (typeof doc.answers !== "object" || doc.answers === null || Array.isArray(doc.answers)) {
    throw contractFail(label, "нет map answers")
  }
  const answers: SystemOneAnswers = {}
  for (const [id, value] of Object.entries(doc.answers as Record<string, unknown>)) {
    answers[id] = parseAnswer(id, value, label)
  }
  const inputTokens = typeof doc.usage?.input_tokens === "number" ? doc.usage.input_tokens : 0
  return { answers, inputTokens }
}

function parseAnswer(id: string, value: unknown, label: string): SystemOneAnswer {
  if (typeof value !== "object" || value === null) throw contractFail(label, `answers[${id}] не объект`)
  const a = value as Record<string, unknown>
  switch (a.type) {
    case "noul":
      if (typeof a.noul !== "number") throw contractFail(label, `answers[${id}] type=noul без числа noul`)
      return { type: "noul", noul: a.noul }
    case "choice":
      if (typeof a.choice !== "string" || typeof a.probabilities !== "object" || a.probabilities === null) {
        throw contractFail(label, `answers[${id}] type=choice без choice/probabilities`)
      }
      return {
        type: "choice",
        choice: a.choice,
        probabilities: a.probabilities as Record<string, number>,
        confidence: typeof a.confidence === "number" ? a.confidence : undefined,
      }
    case "score":
      if (typeof a.score !== "number") throw contractFail(label, `answers[${id}] type=score без числа score`)
      return {
        type: "score",
        score: a.score,
        legend: asStringMap(a.legend),
        probabilities: asNumberMap(a.probabilities),
        confidence: typeof a.confidence === "number" ? a.confidence : undefined,
      }
    default:
      throw contractFail(label, `answers[${id}] неизвестный type=${String(a.type)}`)
  }
}

const asStringMap = (v: unknown): Record<string, string> | undefined =>
  typeof v === "object" && v !== null ? (v as Record<string, string>) : undefined

const asNumberMap = (v: unknown): Record<string, number> | undefined =>
  typeof v === "object" && v !== null ? (v as Record<string, number>) : undefined

// ——— семантика порога: «вероятность проблемы» по типу ответа ———

/** noul — сама вероятность; choice — max P по не-none критериям (или всем,
 * если none нет); score — 1 − P(«0»/чисто). null — посчитать не из чего. */
export function problemProbability(answer: SystemOneAnswer): number | null {
  if (answer.type === "noul") return answer.noul
  if (answer.type === "choice") {
    const probs = Object.entries(answer.probabilities)
    const nonNone = probs.filter(([label]) => label !== "none")
    const pool = nonNone.length > 0 ? nonNone : probs
    return pool.length > 0 ? Math.max(...pool.map(([, p]) => p)) : null
  }
  const cleanP = answer.probabilities?.["0"]
  return typeof cleanP === "number" ? 1 - cleanP : null
}

export function scoreVerdictLabel(answer: SystemOneAnswer): string | null {
  if (answer.type !== "score" || !answer.legend) return null
  const lo = Math.floor(answer.score)
  const hi = Math.ceil(answer.score)
  const loLabel = answer.legend[String(lo)]
  const hiLabel = answer.legend[String(hi)]
  if (lo === hi || !loLabel) return loLabel ?? null
  return hiLabel ? `${loLabel} → ${hiLabel}` : loLabel
}

// ——— рендер markdown ———

export function renderVerdictSection(verdict: ScreenVerdict, state: string, threshold: number): string {
  const lines: string[] = []
  lines.push(`### Скрин: ${verdict.imageName}`)
  lines.push("")
  lines.push(`Путь: \`${verdict.imagePath}\` · state: ${state} · input_tokens: ${verdict.inputTokens}`)
  lines.push("")
  lines.push("| Вопрос | Тип | Вердикт | P (проблема) | Флаг |")
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
    lines.push("**Сигналы** (P > " + threshold + "):")
    for (const { id, p } of flags.sort((a, b) => b.p - a.p)) {
      lines.push(`- ${id} — P ${round(p)}`)
    }
  } else {
    lines.push(`**Сигналов нет** — ни один вопрос не превысил порог ${threshold}.`)
  }
  return lines.join("\n")
}

function verdictCell(answer: SystemOneAnswer): string {
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
    `## screen_review (${meta.model}, чек-лист ${meta.checklistName}, порог ${threshold})`,
    "",
  ]
  const tail = [
    "",
    "> Роль тула — скринер/прицел, не финальный судья: флаги — куда смотреть глазами; финальное суждение — за агентом/человеком.",
  ]
  return [...head, ...sections, ...tail].join("\n")
}
