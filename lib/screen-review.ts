// lib/screen-review.ts — оркестрация тулa screen_review: чтение скринов с
// диска (абсолютные пути на машине сервера opencode — cwd тулa это сервер,
// не сессия агента, готч qlty_review), per-скрин вызов Google Decisions API
// (gemini-decision-1, миграция с локальной олямы #1362 — чистая замена,
// решение владельца 19.11; семантика мульти-скрина — решение ревью плана
// 13.11: один вызов на скрин, вердикты атрибутированы к скрину), сборка
// отчёта. Регистрация — plugins/screen-review.ts. Сеть и ФС подменяются
// deps — юнит-тесты без живого API.
import { basename, isAbsolute } from "node:path"
import {
  buildRequest,
  DEFAULT_CHECKLIST,
  DEFAULT_STATE,
  DEFAULT_THRESHOLD,
  parseAnswers,
  problemProbability,
  resolveMimeType,
  resolveQuestions,
  type Checklist,
  type DecisionRequest,
  type ScreenVerdict,
} from "./screen-review-core"
import { renderReport, renderVerdictSection } from "./screen-review-render"

export const DECISIONS_URL = "https://generativelanguage.googleapis.com/v1beta/decisions"
export const DEFAULT_MODEL = "gemini-decision-1"
/** Латенс API ~0.3–1 с на скрин (проба 20.11); 30 с — запас на большие скрины. */
export const REQUEST_TIMEOUT_MS = 30_000

export interface ScreenReviewInput {
  images: string[]
  state?: string
  checklist?: string
  questions?: Checklist
  threshold?: number
}

export interface ScreenReviewConfig {
  apiKey: string
  model: string
}

export const resolveConfig = (env: NodeJS.ProcessEnv = process.env): ScreenReviewConfig => ({
  apiKey: env.GEMINI_API_KEY ?? "",
  model: DEFAULT_MODEL,
})

export interface ScreenReviewDeps {
  /** подмена сети в тестах (дефолт — нативный fetch c таймаутом) */
  postJson?: (url: string, body: DecisionRequest, headers: Record<string, string>) => Promise<{ status: number; body: string }>
  /** подмена чтения файла в тестах (дефолт — fs/promises readFile → base64) */
  readImageBase64?: (path: string) => Promise<string>
  /** прогресс для агента (плагин кладёт context.progress) — после каждого скрина */
  onProgress?: (status: string) => Promise<void>
  config?: ScreenReviewConfig
}

const defaultPostJson = async (
  url: string,
  body: DecisionRequest,
  headers: Record<string, string>,
): Promise<{ status: number; body: string }> => {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", ...headers },
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
    throw new Error("screen_review: images — a non-empty array of screenshot paths (absolute paths on the server's machine)")
  }
  const notAbsolute = input.images.filter((p) => !isAbsolute(p))
  if (notAbsolute.length > 0) {
    throw new Error(
      `screen_review: screenshot paths must be absolute (the tool cwd is the opencode server, not the agent session): ${notAbsolute.join(", ")}`,
    )
  }
  const questions = resolveQuestions(input)
  const checklistName = input.questions ? "<inline>" : (input.checklist ?? DEFAULT_CHECKLIST)
  const config = deps.config ?? resolveConfig()
  if (!config.apiKey) {
    throw new Error(
      "screen_review: GEMINI_API_KEY is not set — a Gemini API key (Google AI Studio) must be in the machine's env; there is no clean replacement without it (#1362)",
    )
  }
  const state = input.state ?? DEFAULT_STATE
  const threshold = input.threshold ?? DEFAULT_THRESHOLD
  if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
    throw new Error(`screen_review: threshold — a number 0..1 (a probability), got ${threshold}`)
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
    await deps.onProgress?.(`screen_review: ${index + 1}/${input.images.length} — ${verdict.imageName} done`)
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
  const lines = ["### Summary", ""]
  if (flaggedScreens.length === 0) {
    lines.push("No flagged screenshots.")
  } else {
    lines.push(`Flagged screenshots (look with your own eyes first): ${flaggedScreens.map((n) => `\`${n}\``).join(", ")}.`)
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
  const mimeType = resolveMimeType(imagePath) // громкая ошибка на неизвестном расширении
  let imageBase64: string
  try {
    imageBase64 = await (deps.readImageBase64 ?? defaultReadImageBase64)(imagePath)
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e)
    throw new Error(`screen_review: screenshot unreadable — ${imagePath}: ${msg}`)
  }
  const request = buildRequest(config.model, state, imageBase64, mimeType, questions)
  const headers = { "x-goog-api-key": config.apiKey }
  let response: { status: number; body: string }
  try {
    response = await (deps.postJson ?? defaultPostJson)(DECISIONS_URL, request, headers)
  } catch (e) {
    // connection refused / DNS / таймаут AbortSignal — HTTP-статуса нет,
    // причина в e.cause: без обёртки агент видит голое «fetch failed»
    // (паттерн lib/qlty-review.ts — отказы внешних вызовов с контекстом)
    const msg = e instanceof Error ? e.message : String(e)
    const cause = (e as { cause?: unknown }).cause
    const causeMsg = cause === undefined || cause === null ? "" : ` (cause: ${describeCause(cause)})`
    throw new Error(`screen_review: the Gemini Decisions API is unreachable — ${imagePath} → ${DECISIONS_URL}: ${msg}${causeMsg}`)
  }
  if (response.status !== 200) {
    throw new Error(`screen_review: decisions api ${response.status} on ${imagePath} — ${truncate(response.body)}`)
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
