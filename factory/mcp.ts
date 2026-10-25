// mcp.ts — минимальный JSON-RPC клиент MCP-шлюза AFFiNE для демона.
// Сессия одна на процесс; tool-ошибки ({ok:false} внутри HTTP 200) — исключения.

const URL_ = process.env.UWUWU_AFFINE_URL ?? "https://affine-mcp-production.up.railway.app/mcp"
const TOKEN = process.env.UWUWU_AFFINE_TOKEN ?? ""
const WORKSPACE = process.env.UWUWU_AFFINE_WORKSPACE_ID ?? ""

if (TOKEN === "" || WORKSPACE === "") {
  console.error("mcp: нужны UWUWU_AFFINE_TOKEN и UWUWU_AFFINE_WORKSPACE_ID")
  process.exit(1)
}

let session: string | null = null
let nextId = 0

async function rpc(method: string, params: unknown): Promise<unknown> {
  const headers: Record<string, string> = {
    authorization: `Bearer ${TOKEN}`,
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
  }
  if (session !== null) headers["mcp-session-id"] = session
  const res = await fetch(URL_, {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id: ++nextId, method, params }),
    signal: AbortSignal.timeout(60_000),
  })
  const sid = res.headers.get("mcp-session-id")
  if (sid !== null) session = sid
  if (!res.ok) throw new Error(`mcp HTTP ${res.status}`)
  const text = await res.text()
  for (const line of text.split("\n")) {
    if (!line.startsWith("data: ")) continue
    const msg = JSON.parse(line.slice(6)) as {
      result?: { isError?: boolean; content?: { type: string; text?: string }[]; structuredContent?: unknown; error?: unknown }
    }
    if (msg.result === undefined) continue
    if (msg.result.isError) {
      throw new Error(`mcp ${method}: ${msg.result.content?.[0]?.text?.slice(0, 200) ?? "tool error"}`)
    }
    const sc = msg.result.structuredContent as Record<string, unknown> | undefined
    if (sc !== undefined) {
      if (sc.ok === false) throw new Error(`mcp ${method}: ${String(sc.error).slice(0, 200)}`)
      return sc
    }
    for (const c of msg.result.content ?? []) {
      if (c.type === "text" && c.text !== undefined) {
        try {
          const parsed = JSON.parse(c.text) as Record<string, unknown>
          if (parsed.ok === false) throw new Error(`mcp ${method}: ${String(parsed.error).slice(0, 200)}`)
          return parsed
        } catch (e) {
          if (e instanceof SyntaxError) return { text: c.text }
          throw e
        }
      }
    }
    return {}
  }
  return {}
}

export async function connect(): Promise<void> {
  await rpc("initialize", {
    protocolVersion: "2025-03-26",
    capabilities: {},
    clientInfo: { name: "uwuwu-factory", version: "0.1.0" },
  })
  await fetch(URL_, {
    method: "POST",
    headers: {
      authorization: `Bearer ${TOKEN}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "mcp-session-id": session ?? "",
    },
    body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
  }).catch(() => {})
}

export async function tool<T = Record<string, unknown>>(name: string, args: Record<string, unknown>): Promise<T> {
  if (session === null) await connect()
  return (await rpc("tools/call", { name, arguments: { workspaceId: WORKSPACE, ...args } })) as T
}
