import { Plugin } from "@opencode-ai/plugin"
import os from "node:os"
import path from "node:path"

const WIKI = path.join(os.homedir(), ".uwuwu")
const PY = path.join(WIKI, ".venv", "Scripts", "python.exe")
const SCRIPT = path.join(WIKI, "tools", "memvid_query.py")
const TIMEOUT_MS = 90_000

interface SearchInput {
  query: string
  project?: string
  k?: number
  mode?: "auto" | "sem" | "lex"
}

interface ReadInput {
  uri: string
  project?: string
}

interface ExecContext {
  readonly sessionID: string
}

/** Запуск memvid_query.py; resolve — stdout (JSON), reject — человекочитаемая ошибка. */
function runQuery(args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = Bun.spawn([PY, SCRIPT, ...args], {
      stdout: "pipe",
      stderr: "pipe",
      stdin: "ignore",
      windowsHide: true,
    })
    let settled = false
    const timer = setTimeout(() => {
      if (settled) return
      settled = true
      child.kill()
      reject(new Error(`memvid: timeout after ${TIMEOUT_MS / 1000}s (ollama холодный старт?)`))
    }, TIMEOUT_MS)

    const stdout = new Response(child.stdout).text()
    const stderr = new Response(child.stderr).text()

    child.exited
      .then(async (code) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        if (code === 0) {
          resolve(await stdout)
        } else {
          reject(new Error((await stderr).trim() || `memvid_query exited with ${code}`))
        }
      })
      .catch((err: unknown) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        reject(err instanceof Error ? err : new Error(String(err)))
      })
  })
}

/** Память текущего проекта: basename(directory сессии) → .memvid/<name>.mv2. */
async function projectOfSession(ctx: unknown, sessionID: string): Promise<string> {
  try {
    const client = ctx as {
      session: { get: (id: string) => Promise<{ directory?: string }> }
    }
    const s = await client.session.get(sessionID)
    const dir = s.directory ?? ""
    return path.basename(dir) || "redmi"
  } catch {
    return "redmi"
  }
}

export default Plugin.define({
  id: "uwuwu.memvid",
  setup: async (ctx) => {
    await ctx.tool.transform((tools) => {
      tools.add({
        name: "search",
        description:
          "Search past opencode agent sessions stored in memvid project memories " +
          "(~/.uwuwu/.memvid/<project>.mv2). Use it to recall how similar problems were " +
          "solved before, find prior decisions and gotchas. Modes: auto (hybrid BM25+vector, " +
          "default, needs ollama), sem (vector), lex (exact keywords, ids, error codes — " +
          "no ollama needed). Falls back to lex automatically if ollama is down.",
        input: {
          type: "object",
          properties: {
            query: {
              type: "string",
              description: "Search query: natural language or exact keywords",
            },
            project: {
              type: "string",
              description:
                "Memory name (default: memory of the current project — basename of the " +
                "session directory). 'all' searches every project memory.",
            },
            k: {
              type: "integer",
              description: "Top results to return (default 5)",
              minimum: 1,
              maximum: 20,
            },
            mode: {
              type: "string",
              enum: ["auto", "sem", "lex"],
              description: "auto = hybrid (default), sem = vector, lex = BM25 keywords",
            },
          },
          required: ["query"],
          additionalProperties: false,
        },
        options: { namespace: "memvid", codemode: false },
        execute: async (input: SearchInput, exec: ExecContext) => {
          const project = input.project ?? (await projectOfSession(ctx, exec.sessionID))
          const args = ["search", "--query", input.query, "--project", project]
          if (input.k !== undefined) args.push("--k", String(input.k))
          if (input.mode !== undefined) args.push("--mode", input.mode)
          return { content: await runQuery(args) }
        },
      })

      tools.add({
        name: "read",
        description:
          "Read the full text of a memvid frame by uri (returned by memvid.search). " +
          "Returns frame metadata and the complete payload text.",
        input: {
          type: "object",
          properties: {
            uri: {
              type: "string",
              description:
                "Frame uri from memvid.search results, e.g. opencode/2026-08-20/ses_xxx#page-3",
            },
            project: {
              type: "string",
              description: "Memory name where the frame lives (default: current project)",
            },
          },
          required: ["uri"],
          additionalProperties: false,
        },
        options: { namespace: "memvid", codemode: false },
        execute: async (input: ReadInput, exec: ExecContext) => {
          const project = input.project ?? (await projectOfSession(ctx, exec.sessionID))
          const args = ["get", "--project", project, "--uri", input.uri]
          return { content: await runQuery(args) }
        },
      })
    })
  },
})
