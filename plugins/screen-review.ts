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
          "Ревью скринов через decision-модель Gemini Decision 1 (Google Decisions API): чек-лист качества — вероятности и severity по каждому пункту. Скринер/прицел, не судья.",
      })
      editor.add({
        name: "review",
        description:
          "Прогнать скрины через чек-лист качества (дефолт ui-slop: двойная рамка, карточка-в-карточке, выравнивание, налезания/обрезка, контраст + главный дефект + severity). Один вызов модели на скрин — вердикты атрибутированы к скрину. Возвращает markdown-отчёт: таблица вероятностей с флагами P>threshold (дефолт 0.6) и сводка «куда смотреть». Честные границы: модель пропускает часть реальных косяков — флаги это прицел, финальное суждение за тобой (своими глазами/руками). Ограничения вопросов: id [A-Za-z0-9_-]{1,64}; criteria обязательны у choice (map 2–255 опций) и score (массив уровней).",
        input: {
          type: "object",
          properties: {
            images: {
              type: "array",
              items: { type: "string" },
              description:
                "Абсолютные пути к файлам скринов (PNG/JPEG/WebP) на машине сервера opencode (cwd тула — сервер, не сессия агента). Минимум один.",
            },
            state: {
              type: "string",
              description: "Описание контекста экрана для модели (что за экран, что проверяем).",
            },
            checklist: {
              type: "string",
              description: "Имя встроенного чек-листа (дефолт ui-slop).",
            },
            questions: {
              type: "object",
              description:
                "Разовые вопросы вместо встроенного чек-листа: map id → {type: noul|choice|score, instructions, criteria?}. id — [A-Za-z0-9_-]{1,64}. criteria у choice — объект label→описание (2–255 опций, обязателен), у score — массив уровней 2–10 от меньшего к большему (обязателен), у noul — опционально {true_description, false_description}. Одновременный checklist и questions — ошибка.",
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
              description: "Порог флага «вероятности проблемы» (дефолт 0.6 — калибровка пилота pblpp97).",
            },
          },
          required: ["images"],
          additionalProperties: false,
        },
        options: { namespace: "screen", codemode: true },
        execute: async (input, context) => {
          const i = input as ReviewToolInput
          await context.progress({ status: `screen_review: ${i.images.length} скрин(ов) через gemini-decision-1` })
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
            await context.progress({ status: "screen_review: отчёт готов" })
            return { content: r }
          } catch (e) {
            const msg = e instanceof Error ? e.message : String(e)
            return { content: `screen_review: ошибка — ${msg}` }
          }
        },
      })
    })
  },
})
