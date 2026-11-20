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
  "answers[<id>].noul — для type=noul",
  "answers[<id>].choice + answers[<id>].probabilities — для type=choice",
  "answers[<id>].score — для type=score",
] as const

export const DEFAULT_THRESHOLD = 0.6 // порог флага — из пилота pblpp97
export const DEFAULT_STATE = "Скриншот пользовательского интерфейса"
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
    `${label}: контракт ответа Decisions API нарушен — ${what}. Ожидаемые поля: ${ANSWER_CONTRACT_FIELDS.join(", ")}. Gemini сменила формат?`,
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

// ——— валидация входа: всё, на что API отвечает непрозрачным 400,
// ловим честной ошибкой входа до сети (принцип #1362) ———

const inputFail = (id: string, what: string): Error =>
  new Error(`screen_review: вопрос «${id}» — ${what}`)

function validateQuestion(id: string, q: ScreenQuestion): void {
  if (!QUESTION_ID_PATTERN.test(id)) {
    throw new Error(
      `screen_review: id вопроса «${id}» не подходит под ограничения Decisions API — нужен [A-Za-z0-9_-]{1,64}`,
    )
  }
  if (!q.instructions || typeof q.instructions !== "string") {
    throw inputFail(id, "instructions обязателен")
  }
  if (q.type === "noul") {
    // criteria опциональны; при наличии — shape true_description/false_description
    const criteria = q.criteria
    if (criteria === undefined) return
    if (Array.isArray(criteria)) throw inputFail(id, "criteria у noul — объект {true_description, false_description}, не массив")
    const keys = Object.keys(criteria)
    const ok = keys.length === NOUL_CRITERIA_KEYS.length && NOUL_CRITERIA_KEYS.every((k) => typeof criteria[k] === "string")
    if (!ok) {
      throw inputFail(id, `criteria у noul — объект с ключами ${NOUL_CRITERIA_KEYS.join(" и ")}`)
    }
    return
  }
  if (q.criteria === undefined) {
    throw inputFail(id, `criteria обязательны для type=${q.type} (API отвечает 400 без них)`)
  }
  if (q.type === "choice") {
    if (Array.isArray(q.criteria)) throw inputFail(id, "criteria у choice — map label→описание, не массив")
    const labels = Object.keys(q.criteria)
    if (labels.length < MIN_OPTIONS || labels.length > MAX_OPTIONS) {
      throw inputFail(id, `choice — ${MIN_OPTIONS}–${MAX_OPTIONS} опций, получено ${labels.length}`)
    }
    const badKey = labels.find((k) => k.length < 1 || k.length > MAX_OPTION_KEY_LENGTH)
    if (badKey !== undefined) {
      const shown = badKey.length === 0 ? "«» (пустой)" : `«${badKey.slice(0, 24)}…»`
      throw inputFail(id, `ключ опции ${shown} вне лимита 1–${MAX_OPTION_KEY_LENGTH} символов (API отвечает 400)`)
    }
    return
  }
  // score
  if (!Array.isArray(q.criteria)) throw inputFail(id, "criteria у score — массив уровней от меньшего к большему")
  if (q.criteria.length < MIN_OPTIONS || q.criteria.length > MAX_OPTIONS) {
    throw inputFail(id, `score — ${MIN_OPTIONS}–${MAX_OPTIONS} уровней, получено ${q.criteria.length}`)
  }
}

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
  for (const [id, q] of Object.entries(resolved)) validateQuestion(id, q)
  return resolved
}

// ——— сборка запроса ———

export function resolveMimeType(path: string): string {
  const ext = path.slice(path.lastIndexOf(".")).toLowerCase()
  if (ext === ".png") return "image/png"
  if (ext === ".jpg" || ext === ".jpeg") return "image/jpeg"
  if (ext === ".webp") return "image/webp"
  throw new Error(`screen_review: неизвестное расширение скрина «${path}» (поддерживаются .png, .jpg/.jpeg, .webp)`)
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
    throw contractFail(label, "тело ответа не JSON")
  }
  if (typeof doc.answers !== "object" || doc.answers === null || Array.isArray(doc.answers)) {
    throw contractFail(label, "нет map answers")
  }
  const answers: DecisionAnswers = {}
  for (const [id, value] of Object.entries(doc.answers as Record<string, unknown>)) {
    answers[id] = parseAnswer(id, value, label)
  }
  const missing = (expectedIds ?? []).filter((id) => !(id in answers))
  if (missing.length > 0) {
    throw contractFail(label, `модель ответила не на все вопросы — отсутствуют: ${missing.join(", ")}`)
  }
  const usage = doc.usage?.total_input_tokens ?? doc.usage?.input_tokens
  const inputTokens = typeof usage === "number" ? usage : 0
  return { answers, inputTokens }
}

function parseAnswer(id: string, value: unknown, label: string): DecisionAnswer {
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
