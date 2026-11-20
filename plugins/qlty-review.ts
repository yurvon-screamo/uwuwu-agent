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
          "Algorithmic machine review on the qlty engine: linter plugins + smells, a native line-level delta against upstream. Independent of gh.",
      })
      editor.add({
        name: "review",
        description:
          "Machine qlty review of a diff or a commit: qlty check/smells --upstream — findings only in changed lines (a file's pre-existing baseline debt is not shown; the uncommitted tail and untracked files are part of the delta). Attribution format [plugin:rule] path:line (level) — message; a whole-file finding without region.startLine — path (строка неизвестна) (level). Returns a markdown report. The default level low cuts the note noise.",
        input: {
          type: "object",
          properties: {
            scope: {
              type: "string",
              enum: ["diff", "commit"],
              description:
                "diff — the working copy against upstream (the branch delta + untracked + the uncommitted tail); commit — the delta of one commit against its parent (a run over the working copy).",
            },
            directory: {
              type: "string",
              description:
                "Path to the qlty repo. Required if the opencode server's cwd is not the target repo (the tool's cwd is the server, not the agent's session).",
            },
            commit: {
              type: "string",
              description: "Commit SHA/ref for scope=commit.",
            },
            upstream: {
              type: "string",
              description: "The base ref for the line-level delta (its merge-base is the run's base). Default origin/master.",
            },
            paths: {
              type: "array",
              items: { type: "string" },
              description: "Glob path filter (*, **, ?). Empty — all changed paths.",
            },
            level: {
              type: "string",
              enum: ["note", "low", "medium", "high"],
              description:
                "Finding threshold: note — show everything; low — cuts SARIF-note (default); medium/high — keep only SARIF-error (warning/error are not distinguished in SARIF by source severity, error is never cut).",
            },
          },
          required: ["scope"],
          additionalProperties: false,
        },
        options: { namespace: "qlty", codemode: true },
        execute: async (input, context) => {
          const i = input as ReviewToolInput
          const dir = i.directory ?? process.cwd()
          await context.progress({ status: `qlty_review: git+check/smells in ${dir}` })
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
            return { content: `qlty_review: error — ${msg}` }
          }
        },
      })
    })
  },
})
