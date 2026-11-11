// plugins/qlty-review.ts — тул qlty_review для OpenCode: машинный
// алгоритмический ревью (qlty check/smells --upstream, нативная строковая
// дельта против upstream). Тонкая регистрация — вся логика
// в ../lib/qlty-review (тестируется отдельно). Ниша: пре-пуш фаза
// девелопера и верификация ворктри ревьювером (ADR-063 — CI видит только
// запушенную дельту; незакоммиченный хвост и untracked в тул попадают).
// Санкция владельца: «тулу в опенкоде плагином... газ»
// (задача 20261106182052-k2orqlz).

import { Plugin } from "@opencode/plugin"
import { runReview, type ReviewInput } from "../lib/qlty-review"

interface ReviewToolInput {
  scope: "diff" | "commit"
  directory?: string
  commit?: string
  upstream?: string
  paths?: string[]
  level?: "note" | "low" | "medium" | "high"
}

export default Plugin.define({
  id: "qlty-review",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.namespace({
        name: "qlty",
        description:
          "Алгоритмический машинный ревью на движке qlty: линтеры-плагины + smells, нативная строковая дельта против upstream. Не зависит от gh.",
      })
      editor.add({
        name: "review",
        description:
          "Машинный qlty-ревью дифа или коммита: qlty check/smells --upstream — находки только в изменённых строках (pre-existing baseline-долг файла не показывается; незакоммиченный хвост и untracked в дельту входят). Маркетинговый формат атрибуции [plugin:rule] path:line (level) — message; whole-file находка без region.startLine — path (строка неизвестна) (level). Возвращает markdown-отчёт. Дефолтный уровень low отсекает note-шум.",
        input: {
          type: "object",
          properties: {
            scope: {
              type: "string",
              enum: ["diff", "commit"],
              description:
                "diff — рабочая копия против upstream (дельта ветки + untracked + незакоммиченный хвост); commit — дельта одного коммита против его родителя (прогон по рабочей копии).",
            },
            directory: {
              type: "string",
              description:
                "Путь к qlty-репо. Обязателен, если cwd сервера opencode — не целевой репо (cwd тула — сервер, не сессия агента).",
            },
            commit: {
              type: "string",
              description: "SHA/реф коммита для scope=commit.",
            },
            upstream: {
              type: "string",
              description: "Базовый реф для строковой дельты (merge-base с ним — база прогона). Дефолт origin/master.",
            },
            paths: {
              type: "array",
              items: { type: "string" },
              description: "Глоб-фильтр путей (*, **, ?). Пусто — все изменённые пути.",
            },
            level: {
              type: "string",
              enum: ["note", "low", "medium", "high"],
              description:
                "Порог находок: note — показывать всё; low — отсекает SARIF-note (дефолт); medium/high — оставляют только SARIF-error (warning/error в SARIF не различаются по исходной severity, error не отсекается никогда).",
            },
          },
          required: ["scope"],
          additionalProperties: false,
        },
        options: { namespace: "qlty", codemode: true },
        execute: async (input, context) => {
          const i = input as ReviewToolInput
          const dir = i.directory ?? process.cwd()
          await context.progress({ status: `qlty_review: git+check/smells в ${dir}` })
          try {
            const req: ReviewInput = {
              scope: i.scope,
              directory: dir,
              upstream: i.upstream,
              commit: i.commit,
              paths: i.paths,
              level: i.level,
            }
            const r = await runReview(req)
            await context.progress({
              status: `qlty_review: ${r.delta} delta + ${r.smells} smells`,
            })
            return { content: r.markdown }
          } catch (e) {
            const msg = e instanceof Error ? e.message : String(e)
            return { content: `qlty_review: ошибка — ${msg}` }
          }
        },
      })
    })
  },
})
