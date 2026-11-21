// plugins/screen-review.ts — тул screen_review для OpenCode: ревью скринов
// через decision-модель Gemini Decision 1 (Google Decisions API; миграция с
// локальной d1-omni #1362 — чистая замена, решение владельца 19.11). Тонкая
// регистрация — вся логика в ../lib/screen-review (тестируется отдельно).
// Ниша: скринер/прицел для девелопера (самопроверка экрана до PR),
// тестировщика (первичный обход скринов) и ревьювера (структурная проверка
// присланных скринов). Финальное суждение — за агентом/человеком: тул
// ловит грубые слоп-паттерны и ранжирует, но пропускает часть реальных
// косяков (пилот pblpp97: двойные рамки /download при «none» 0.61).
// Санкция владельца: «сделать опенкод тулу — которая будет врапером над
// d1-omni и разработчик, тестировщик, ревьювер будут через нее вручную
// проверять свои скрины» (задача 20261112232632-hlp4no2, 12.11).

import { Plugin } from "@opencode/plugin"
import { runScreenReview, type ScreenReviewInput } from "../lib/screen-review"

interface ReviewToolInput {
  images: string[]
  state?: string
  checklist?: string
  questions?: {
    [id: string]: {
      type: "noul" | "choice" | "score"
      instructions: string
      criteria?: Record<string, string> | string[]
    }
  }
  threshold?: number
}

export default Plugin.define({
  id: "screen-review",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.namespace({
        name: "screen",
        description:
          "Screenshot review via the Gemini Decision 1 decision model (Google Decisions API): a quality checklist — probabilities and severity per item. A screener/aimer, not a judge.",
      })
      editor.add({
        name: "review",
        description:
          "Run screenshots through the quality checklist (default ui-slop: double frame, card-in-card, alignment, overlaps/clipping, contrast + main defect + severity). One model call per screenshot — verdicts attributed to the screenshot. Returns a markdown report: a probability table with P>threshold flags (default 0.6) and a «where to look» digest. Honest limits: the model misses some real faults — the flags are an aim, the final judgment is yours (with your own eyes/hands). Question constraints: id [A-Za-z0-9_-]{1,64}; criteria are required for choice (a map of 2–255 options) and score (an array of levels).",
        input: {
          type: "object",
          properties: {
            images: {
              type: "array",
              items: { type: "string" },
              description:
                "Absolute paths to screenshot files (PNG/JPEG/WebP) on the opencode server's machine (the tool's cwd is the server, not the agent's session). At least one.",
            },
            state: {
              type: "string",
              description: "The screen context description for the model (what screen it is, what we are checking).",
            },
            checklist: {
              type: "string",
              description: "The built-in checklist name (default ui-slop).",
            },
            questions: {
              type: "object",
              description:
                "One-off questions instead of the built-in checklist: a map id → {type: noul|choice|score, instructions, criteria?}. id — [A-Za-z0-9_-]{1,64}. criteria for choice — a label→description object (2–255 options, required), for score — an array of levels 2–255 ascending (required), for noul — optional {true_description, false_description}. checklist and questions at the same time — an error.",
              additionalProperties: {
                type: "object",
                properties: {
                  type: { type: "string", enum: ["noul", "choice", "score"] },
                  instructions: { type: "string" },
                  criteria: { type: ["object", "array"] },
                },
                required: ["type", "instructions"],
              },
            },
            threshold: {
              type: "number",
              description: "The «problem probability» flag threshold (default 0.6 — the pblpp97 pilot calibration).",
            },
          },
          required: ["images"],
          additionalProperties: false,
        },
        options: { namespace: "screen", codemode: true },
        execute: async (input, context) => {
          const i = input as ReviewToolInput
          await context.progress({ status: `screen_review: ${i.images.length} screenshot(s) via gemini-decision-1` })
          try {
            const req: ScreenReviewInput = {
              images: i.images,
              state: i.state,
              checklist: i.checklist,
              questions: i.questions,
              threshold: i.threshold,
            }
            const r = await runScreenReview(req, {
              onProgress: (status) => context.progress({ status }),
            })
            await context.progress({ status: "screen_review: the report is ready" })
            return { content: r }
          } catch (e) {
            const msg = e instanceof Error ? e.message : String(e)
            return { content: `screen_review: error — ${msg}` }
          }
        },
      })
    })
  },
})
