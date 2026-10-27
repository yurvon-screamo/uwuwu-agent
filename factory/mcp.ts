// mcp.ts — минимальный JSON-RPC клиент MCP-шлюза AFFiNE для демона.
// Сессия одна на процесс; tool-ошибки ({ok:false} внутри HTTP 200) — исключения.
// Протухшая сессия (шлюз редеплоился → 404) — сброс + один ретрай: демон
// long-running, слепнуть навсегда нельзя.

const URL_ = process.env.UWUWU_AFFINE_URL ?? "http://127.0.0.1:8000/mcp"
const TOKEN = process.env.UWUWU_AFFINE_TOKEN ?? ""
const WORKSPACE = process.env.UWUWU_AFFINE_WORKSPACE_ID ?? ""

if (TOKEN === "" || WORKSPACE === "") {
  console.error("mcp: нужны UWUWU_AFFINE_TOKEN и UWUWU_AFFINE_WORKSPACE_ID")
  process.exit(1)
}

let session: string | null = null
let nextId = 0

class SessionExpired extends Error {}

async function rpc(method: string, params: unknown, sid: string): Promise<unknown> {
  const headers: Record<string, string> = {
    authorization: `Bearer ${TOKEN}`,
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
    "mcp-session-id": sid,
  }
  const res = await fetch(URL_, {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id: ++nextId, method, params }),
    signal: AbortSignal.timeout(60_000),
  })
  if (res.status === 404 || res.status === 400) throw new SessionExpired(`HTTP ${res.status}`)
  if (!res.ok) throw new Error(`mcp HTTP ${res.status}`)
  const text = await res.text()
  for (const line of text.split("\n")) {
    if (!line.startsWith("data: ")) continue
    let msg: {
      result?: { isError?: boolean; content?: { type: string; text?: string }[]; structuredContent?: unknown }
    }
    try {
      msg = JSON.parse(line.slice(6))
    } catch {
      continue // мусорная SSE-строка — не роняем тик
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

async function handshake(): Promise<string> {
  const res = await fetch(URL_, {
    method: "POST",
    headers: {
      authorization: `Bearer ${TOKEN}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: ++nextId,
      method: "initialize",
      params: { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "uwuwu-factory", version: "0.1.0" } },
    }),
    signal: AbortSignal.timeout(60_000),
  })
  if (!res.ok) throw new Error(`mcp initialize HTTP ${res.status}`)
  const sid = res.headers.get("mcp-session-id")
  if (sid === null) throw new Error("mcp initialize: нет mcp-session-id")
  await fetch(URL_, {
    method: "POST",
    headers: {
      authorization: `Bearer ${TOKEN}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "mcp-session-id": sid,
    },
    body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
  }).catch(() => {})
  return sid
}

export async function connect(): Promise<void> {
  session = await handshake()
}

export async function tool<T = Record<string, unknown>>(name: string, args: Record<string, unknown>): Promise<T> {
  if (session === null) session = await handshake()
  const call = (sid: string): Promise<unknown> =>
    rpc("tools/call", { name, arguments: { workspaceId: WORKSPACE, ...args } }, sid)
  try {
    return (await call(session)) as T
  } catch (e) {
    // протухшая сессия (редеплой шлюза) ИЛИ транспортный сбой
    // (отравленный keep-alive сокет у долгоживущего процесса) —
    // обе лечатся полной пересборкой сессии = новое соединение
    session = null
    session = await handshake()
    return (await call(session)) as T
  }
}

/** Только для тестов: подменить сессию битой (симуляция редеплоя шлюза). */
export const __test = {
  expireSession(): void {
    session = "bogus-session-id"
  },
}
