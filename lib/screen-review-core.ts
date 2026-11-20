// lib/screen-review-core.ts — чистые функции ядра screen_review (тул ревью
// скринов над decision-моделью Gemini Decision 1 через Google Decisions API,
// миграция с локальной d1-omni #1362): встроенный чек-лист ui-slop, сборка
// запроса (input-блоки text+image), парсинг ответа с контрактом полей
// (анти-дрейф), валидация входа по ограничениям API, рендер markdown-отчёта.
// Без сети и ФС — тестируется напрямую (screen-review.test.ts), оркестрация —
// screen-review.ts. Форма — по прецеденту qlty_review (ADR-068). Живой
// контракт Decisions API зафиксирован пробами 20.11.2026 (#1362, срез 1):
// answers один-в-один с прежним systemone; id вопроса [A-Za-z0-9_-]{1,64};
// ключ опций choice ≤128 символов (400 живьём), charset свободный
// (пробел/кириллица приняты); noul-criteria {true_description,
// false_description} приняты; criteria обязательны для choice/score.
//
// Семантика мульти-скрина (решение ревью плана 13.11): пер-скрин прогон —
// один вызов API на скрин, одинаковые вопросы; отчёт носит секцию на
// каждый скрин — вердикты атрибутированы к скрину нативно.

// ——— типы ———

export type QuestionType = "noul" | "choice" | "score"

/** Вопрос Decisions API: instructions — текст, criteria — опции choice
 * (label→описание), уровни score (массив от меньшего к большему) или
 * смыслы noul ({true_description, false_description}). */
export interface ScreenQuestion {
  type: QuestionType
  instructions: string
  criteria?: Record<string, string> | string[]
}

/** Чек-лист — map id→вопрос (API принимает 1–1000 вопросов; кап тула — 64). */
export type Checklist = Record<string, ScreenQuestion>

/** Блок input: текст (state) или изображение (base64 + mime). */
export type DecisionInputPart =
  | { type: "text"; text: string }
  | { type: "image"; data: string; mime_type: string }

export interface DecisionRequest {
  model: string
  input: DecisionInputPart[]
  questions: Checklist
}

export type DecisionAnswer =
  | { type: "noul"; noul: number }
  | { type: "choice"; choice: string; probabilities: Record<string, number>; confidence?: number }
  | {
      type: "score"
      score: number
      legend?: Record<string, string>
      probabilities?: Record<string, number>
      confidence?: number
    }

export type DecisionAnswers = Record<string, DecisionAnswer>

export interface ScreenVerdict {
  /** путь к скрину, как передан на входе тула */
  imagePath: string
  /** base-имя файла для заголовка секции отчёта */
  imageName: string
  answers: DecisionAnswers
  inputTokens: number
}

// ——— контракты (анти-дрейф): нехватка поля = громкая ошибка ———

export const ANSWER_CONTRACT_FIELDS = [
  "answers[<id>].type",
  "answers[<id>].noul — for type=noul",
  "answers[<id>].choice + answers[<id>].probabilities — for type=choice",
  "answers[<id>].score — for type=score",
] as const

export const DEFAULT_THRESHOLD = 0.6 // порог флага — из пилота pblpp97
export const DEFAULT_STATE = "Screenshot of a user interface"
export const DEFAULT_CHECKLIST = "ui-slop"
export const MAX_QUESTIONS = 64 // кап тула; API принимает до 1000

/** Ограничения Decisions API, подтверждённые доками и живыми пробами 20.11 (#1362). */
export const QUESTION_ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/
export const MAX_OPTIONS = 255
export const MIN_OPTIONS = 2
export const MAX_OPTION_KEY_LENGTH = 128 // 400 живьём: «must have an id of at most 128 characters»
export const NOUL_CRITERIA_KEYS = ["true_description", "false_description"] as const

const contractFail = (label: string, what: string): Error =>
  new Error(
    `${label}: the Decisions API answer contract is broken — ${what}. Expected fields: ${ANSWER_CONTRACT_FIELDS.join(", ")}. Did Gemini change its format?`,
  )

// ——— встроенный чек-лист ui-slop (постановка задачи; источник истины
// паттернов — DESIGN.md канон origa, соответствие фиксирует док-заметка) ———

export const CHECKLISTS: Record<string, Checklist> = {
  [DEFAULT_CHECKLIST]: {
    double_frame: {
      type: "noul",
      instructions: "Do cards or panels have a double frame (a contour inside a contour, a frame inside a frame)?",
    },
    card_in_card: {
      type: "noul",
      instructions: "Is there a card inside a card (a nested panel with a frame or background inside a similar outer one)?",
    },
    alignment: {
      type: "noul",
      instructions: "Is element alignment broken (an element falls out of the grid or row, an unexpected shift)?",
    },
    overlap: {
      type: "noul",
      instructions: "Are there elements overlapping each other or clipped content (text or a block goes beyond bounds, is covered)?",
    },
    contrast: {
      type: "noul",
      instructions: "Are there contrast problems (text is hard to read on the background, too pale, close colors)?",
    },
    main_defect: {
      type: "choice",
      instructions: "What is the screen's main visual defect?",
      criteria: {
        none: "no defects",
        double_frame: "double frames",
        card_in_card: "a card inside a card",
        alignment: "broken alignment",
        overlap: "overlaps or clipping",
        contrast: "contrast problems",
        other: "another defect",
      },
    },
    severity: {
      type: "score",
      instructions: "How severe are the screen's visual defects?",
      criteria: ["clean", "minor flaws", "obvious defects", "critical"],
    },
  },
}

// ——— валидация входа: всё, на что API отвечает непрозрачным 400,
// ловим честной ошибкой входа до сети (принцип #1362) ———

const inputFail = (id: string, what: string): Error =>
  new Error(`screen_review: question «${id}» — ${what}`)

function validateQuestion(id: string, q: ScreenQuestion): void {
  if (!QUESTION_ID_PATTERN.test(id)) {
    throw new Error(
      `screen_review: question id «${id}» violates the Decisions API constraints — [A-Za-z0-9_-]{1,64} required`,
    )
  }
  if (!q.instructions || typeof q.instructions !== "string") {
    throw inputFail(id, "instructions is required")
  }
  if (q.type === "noul") {
    // criteria опциональны; при наличии — shape true_description/false_description
    const criteria = q.criteria
    if (criteria === undefined) return
    if (Array.isArray(criteria)) throw inputFail(id, "criteria for noul — an object {true_description, false_description}, not an array")
    const keys = Object.keys(criteria)
    const ok = keys.length === NOUL_CRITERIA_KEYS.length && NOUL_CRITERIA_KEYS.every((k) => typeof criteria[k] === "string")
    if (!ok) {
      throw inputFail(id, `criteria for noul — an object with the keys ${NOUL_CRITERIA_KEYS.join(" and ")}`)
    }
    return
  }
  if (q.criteria === undefined) {
    throw inputFail(id, `criteria is required for type=${q.type} (the API answers 400 without them)`)
  }
  if (q.type === "choice") {
    if (Array.isArray(q.criteria)) throw inputFail(id, "criteria for choice — a label→description map, not an array")
    const labels = Object.keys(q.criteria)
    if (labels.length < MIN_OPTIONS || labels.length > MAX_OPTIONS) {
      throw inputFail(id, `choice — ${MIN_OPTIONS}–${MAX_OPTIONS} options, got ${labels.length}`)
    }
    const badKey = labels.find((k) => k.length < 1 || k.length > MAX_OPTION_KEY_LENGTH)
    if (badKey !== undefined) {
      const shown = badKey.length === 0 ? "«» (empty)" : `«${badKey.slice(0, 24)}…»`
      throw inputFail(id, `option key ${shown} is outside the 1–${MAX_OPTION_KEY_LENGTH} character limit (the API answers 400)`)
    }
    return
  }
  // score
  if (!Array.isArray(q.criteria)) throw inputFail(id, "criteria for score — an array of levels ascending")
  if (q.criteria.length < MIN_OPTIONS || q.criteria.length > MAX_OPTIONS) {
    throw inputFail(id, `score — ${MIN_OPTIONS}–${MAX_OPTIONS} levels, got ${q.criteria.length}`)
  }
}

export function resolveQuestions(input: {
  checklist?: string
  questions?: Checklist
}): Checklist {
  if (input.questions && input.checklist !== undefined) {
    throw new Error("screen_review: both checklist and questions are set — keep one source of questions")
  }
  const checklistName = input.checklist ?? DEFAULT_CHECKLIST
  const resolved = input.questions ?? CHECKLISTS[checklistName]
  if (!resolved) {
    const known = Object.keys(CHECKLISTS).join(", ")
    throw new Error(`screen_review: unknown checklist «${checklistName}» (built-in: ${known}; your own questions — the questions parameter)`)
  }
  const count = Object.keys(resolved).length
  if (count < 1 || count > MAX_QUESTIONS) {
    throw new Error(`screen_review: questions must hold 1–${MAX_QUESTIONS} questions, got ${count}`)
  }
  for (const [id, q] of Object.entries(resolved)) validateQuestion(id, q)
  return resolved
}

// ——— сборка запроса ———

export function resolveMimeType(path: string): string {
  const ext = path.slice(path.lastIndexOf(".")).toLowerCase()
  if (ext === ".png") return "image/png"
  if (ext === ".jpg" || ext === ".jpeg") return "image/jpeg"
  if (ext === ".webp") return "image/webp"
  throw new Error(`screen_review: unknown screenshot extension «${path}» (supported: .png, .jpg/.jpeg, .webp)`)
}

export function buildRequest(
  model: string,
  state: string,
  imageBase64: string,
  mimeType: string,
  questions: Checklist,
): DecisionRequest {
  return {
    model,
    input: [
      { type: "text", text: state },
      { type: "image", data: imageBase64, mime_type: mimeType },
    ],
    questions,
  }
}

// ——— парсинг ответа Decisions API ———

export function parseAnswers(
  raw: string,
  label: string,
  expectedIds?: readonly string[],
): { answers: DecisionAnswers; inputTokens: number } {
  let doc: {
    answers?: unknown
    usage?: { total_input_tokens?: unknown; input_tokens?: unknown }
  }
  try {
    doc = JSON.parse(raw) as typeof doc
  } catch {
    throw contractFail(label, "the response body is not JSON")
  }
  if (typeof doc.answers !== "object" || doc.answers === null || Array.isArray(doc.answers)) {
    throw contractFail(label, "no answers map")
  }
  const answers: DecisionAnswers = {}
  for (const [id, value] of Object.entries(doc.answers as Record<string, unknown>)) {
    answers[id] = parseAnswer(id, value, label)
  }
  const missing = (expectedIds ?? []).filter((id) => !(id in answers))
  if (missing.length > 0) {
    throw contractFail(label, `the model did not answer all questions — missing: ${missing.join(", ")}`)
  }
  const usage = doc.usage?.total_input_tokens ?? doc.usage?.input_tokens
  const inputTokens = typeof usage === "number" ? usage : 0
  return { answers, inputTokens }
}

function parseAnswer(id: string, value: unknown, label: string): DecisionAnswer {
  if (typeof value !== "object" || value === null) throw contractFail(label, `answers[${id}] is not an object`)
  const a = value as Record<string, unknown>
  switch (a.type) {
    case "noul":
      if (typeof a.noul !== "number") throw contractFail(label, `answers[${id}] type=noul without a noul number`)
      return { type: "noul", noul: a.noul }
    case "choice":
      if (typeof a.choice !== "string" || typeof a.probabilities !== "object" || a.probabilities === null) {
        throw contractFail(label, `answers[${id}] type=choice without choice/probabilities`)
      }
      return {
        type: "choice",
        choice: a.choice,
        probabilities: a.probabilities as Record<string, number>,
        confidence: typeof a.confidence === "number" ? a.confidence : undefined,
      }
    case "score":
      if (typeof a.score !== "number") throw contractFail(label, `answers[${id}] type=score without a score number`)
      return {
        type: "score",
        score: a.score,
        legend: asStringMap(a.legend),
        probabilities: asNumberMap(a.probabilities),
        confidence: typeof a.confidence === "number" ? a.confidence : undefined,
      }
    default:
      throw contractFail(label, `answers[${id}] unknown type=${String(a.type)}`)
  }
}

const asStringMap = (v: unknown): Record<string, string> | undefined =>
  typeof v === "object" && v !== null ? (v as Record<string, string>) : undefined

const asNumberMap = (v: unknown): Record<string, number> | undefined =>
  typeof v === "object" && v !== null ? (v as Record<string, number>) : undefined

// ——— семантика порога: «вероятность проблемы» по типу ответа ———

/** noul — сама вероятность; choice — max P по не-none критериям (или всем,
 * если none нет); score — 1 − P(«0»/чисто). null — посчитать не из чего. */
export function problemProbability(answer: DecisionAnswer): number | null {
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
