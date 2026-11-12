// lib/screen-review.ts — оркестрация тулa screen_review: чтение скринов с
// диска (абсолютные пути на машине сервера opencode — cwd тулa это сервер,
// не сессия агента, готч qlty_review), per-скрин вызов /v1/systemone
// локальной олямы (семантика мульти-скрина — решение ревью плана 13.11:
// один вызов на скрин, вердикты атрибутированы к скрину), сборка отчёта.
// Регистрация — plugins/screen-review.ts. Сеть и ФС подменяются deps —
// юнит-тесты без живой модели.
import { basename, isAbsolute } from "node:path"
import {
  buildRequest,
  DEFAULT_CHECKLIST,
  DEFAULT_STATE,
  DEFAULT_THRESHOLD,
  parseAnswers,
  problemProbability,
  resolveQuestions,
  type Checklist,
  type ScreenVerdict,
  type SystemOneRequest,
} from "./screen-review-core"
import { renderReport, renderVerdictSection } from "./screen-review-render"

export const DEFAULT_OLLAMA_URL = "http://127.0.0.1:11434"
export const DEFAULT_MODEL = "d1-omni"
/** Холодный старт модели на CPU — десятки секунд (замер 13.11: ~26 с). */
export const REQUEST_TIMEOUT_MS = 180_000

export interface ScreenReviewInput {
  images: string[]
  state?: string
  checklist?: string
  questions?: Checklist
  threshold?: number
}

export interface ScreenReviewConfig {
  ollamaUrl: string
  model: string
}

export const resolveConfig = (env: NodeJS.ProcessEnv = process.env): ScreenReviewConfig => ({
  ollamaUrl: env.UWUWU_OMNI_URL ?? DEFAULT_OLLAMA_URL,
  model: env.UWUWU_OMNI_MODEL ?? DEFAULT_MODEL,
})

export interface ScreenReviewDeps {
  /** подмена сети в тестах (дефолт — нативный fetch c таймаутом) */
  postJson?: (url: string, body: SystemOneRequest) => Promise<{ status: number; body: string }>
  /** подмена чтения файла в тестах (дефолт — fs/promises readFile → base64) */
  readImageBase64?: (path: string) => Promise<string>
  /** прогресс для агента (плагин кладёт context.progress) — после каждого скрина */
  onProgress?: (status: string) => Promise<void>
  config?: ScreenReviewConfig
}

const defaultPostJson = async (url: string, body: SystemOneRequest): Promise<{ status: number; body: string }> => {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  })
  return { status: response.status, body: await response.text() }
}

const defaultReadImageBase64 = async (path: string): Promise<string> => {
  const { readFile } = await import("node:fs/promises")
  return (await readFile(path)).toString("base64")
}

export async function runScreenReview(input: ScreenReviewInput, deps: ScreenReviewDeps = {}): Promise<string> {
  if (!Array.isArray(input.images) || input.images.length < 1) {
    throw new Error("screen_review: images — непустой массив путей к скринам (абсолютные пути на машине сервера)")
  }
  const notAbsolute = input.images.filter((p) => !isAbsolute(p))
  if (notAbsolute.length > 0) {
    throw new Error(
      `screen_review: пути к скринам должны быть абсолютными (cwd тулa — сервер opencode, не сессия агента): ${notAbsolute.join(", ")}`,
    )
  }
  const questions = resolveQuestions(input)
  const checklistName = input.questions ? "<inline>" : (input.checklist ?? DEFAULT_CHECKLIST)
  const config = deps.config ?? resolveConfig()
  const state = input.state ?? DEFAULT_STATE
  const threshold = input.threshold ?? DEFAULT_THRESHOLD
  if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
    throw new Error(`screen_review: threshold — число 0..1 (вероятность), получено ${threshold}`)
  }

  const sections: string[] = []
  const verdicts: ScreenVerdict[] = []
  for (const [index, imagePath] of input.images.entries()) {
    const verdict = await reviewImage(
      { imagePath, state, questions, config, threshold },
      { postJson: deps.postJson, readImageBase64: deps.readImageBase64 },
    )
    verdicts.push(verdict)
    sections.push(renderVerdictSection(verdict, state, threshold))
    await deps.onProgress?.(`screen_review: ${index + 1}/${input.images.length} — ${verdict.imageName} готов`)
  }
  const flagged = verdicts.filter((v) => sectionHasFlags(v, threshold))
  const summary = renderSummary(flagged.map((v) => v.imageName))
  return renderReport([...sections, summary], { model: config.model, checklistName }, threshold)
}

const sectionHasFlags = (verdict: ScreenVerdict, threshold: number): boolean =>
  Object.values(verdict.answers).some((a) => {
    const p = problemProbability(a)
    return p !== null && p > threshold
  })

function renderSummary(flaggedScreens: string[]): string {
  const lines = ["### Сводка", ""]
  if (flaggedScreens.length === 0) {
    lines.push("Скринов с флагами нет.")
  } else {
    lines.push(`Скрины с флагами (смотреть глазами в первую очередь): ${flaggedScreens.map((n) => `\`${n}\``).join(", ")}.`)
  }
  return lines.join("\n")
}

async function reviewImage(
  args: {
    imagePath: string
    state: string
    questions: Checklist
    config: ScreenReviewConfig
    threshold: number
  },
  deps: Pick<ScreenReviewDeps, "postJson" | "readImageBase64">,
): Promise<ScreenVerdict> {
  const { imagePath, state, questions, config } = args
  let imageBase64: string
  try {
    imageBase64 = await (deps.readImageBase64 ?? defaultReadImageBase64)(imagePath)
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e)
    throw new Error(`screen_review: скрин не читан — ${imagePath}: ${msg}`)
  }
  const url = `${config.ollamaUrl.replace(/\/$/, "")}/v1/systemone`
  const request = buildRequest(config.model, state, imageBase64, questions)
  let response: { status: number; body: string }
  try {
    response = await (deps.postJson ?? defaultPostJson)(url, request)
  } catch (e) {
    // connection refused / DNS / таймаут AbortSignal — HTTP-статуса нет,
    // причина в e.cause: без обёртки агент видит голое «fetch failed»
    // (паттерн lib/qlty-review.ts — отказы внешних вызовов с контекстом)
    const msg = e instanceof Error ? e.message : String(e)
    const cause = (e as { cause?: unknown }).cause
    const causeMsg = cause === undefined || cause === null ? "" : ` (причина: ${describeCause(cause)})`
    throw new Error(`screen_review: оляма недоступна — ${imagePath} → ${url}: ${msg}${causeMsg}`)
  }
  if (response.status !== 200) {
    throw new Error(`screen_review: ollama ${response.status} на ${imagePath} — ${truncate(response.body)}`)
  }
  const label = `screen_review (${basename(imagePath)})`
  const { answers, inputTokens } = parseAnswers(response.body, label, Object.keys(questions))
  return { imagePath, imageName: basename(imagePath), answers, inputTokens }
}

function describeCause(cause: unknown): string {
  if (cause instanceof Error) return cause.message
  if (typeof cause === "string") return cause
  return JSON.stringify(cause)
}

const truncate = (s: string, max = 300): string => (s.length <= max ? s : `${s.slice(0, max)}…`)
