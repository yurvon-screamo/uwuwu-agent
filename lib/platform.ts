// lib/platform.ts — клиент платформы uwuwu (SiYuan).
//
// С 04.11.2026 бекенд — SiYuan (миграция с платформы-предшественника). Файл сохраняет экспортный
// контракт (mcpConfigured/connect/tool), но разговаривает с SiYuan kernel
// (REST) и его встроенным MCP (database-операции). Шейпы ответов эмулируют
// прежние афин-тулзы, чтобы cron/factory/triage работали без правок.
//
// Env: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806), UWUWU_SIYUAN_TOKEN
// (деф. api.token из $UWUWU_SIYUAN_WS/conf/conf.json).

import { readFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { collectBoardView, type BoardViewPage } from "./board-view"

const SIYUAN_URL = process.env.UWUWU_SIYUAN_URL || "http://127.0.0.1:6806"

// Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки stmt
// (см. tests/platform-sql-limit.test.ts) — граница обязательна в каждом stmt.
const SQL_LIMIT = 100000

function siyuanToken(): string {
  const t = process.env.UWUWU_SIYUAN_TOKEN
  if (t && t !== "") return t
  const ws = process.env.UWUWU_SIYUAN_WS || join(homedir(), "siyuan-demo-ws")
  try {
    const conf = JSON.parse(readFileSync(join(ws, "conf/conf.json"), "utf8"))
    return conf?.api?.token ?? ""
  } catch {
    return ""
  }
}

const TOKEN = siyuanToken()

export const mcpConfigured = (): boolean => TOKEN !== ""

// ——— ядро: REST ———

/** JSON.parse с честной ошибкой: HTTP-статус и сниппет тела вместо голого SyntaxError,
 *  маскирующего причину (прод-кейс 04.11: «Unexpected identifier "session"» = 404 ядра). */
function parseJsonContract(text: string, context: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    throw new Error(`${context}: не-JSON: ${text.slice(0, 120)}`)
  }
}

async function post<T = Record<string, unknown>>(path: string, body: Record<string, unknown>, attempts = 4): Promise<T> {
  let last = "?"
  for (let i = 0; i < attempts; i++) {
    try {
      const r = await fetch(SIYUAN_URL + path, {
        method: "POST",
        headers: { Authorization: `Token ${TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify(body),
      })
      const text = await r.text()
      const d = parseJsonContract(text, `siyuan POST ${path}: HTTP ${r.status}`) as { code?: number; msg?: string; data?: unknown }
      if (d.code === 0) return (d.data ?? {}) as T
      last = `code=${d.code} msg=${d.msg}`
      if (String(d.msg).includes("metadata is not ready")) {
        await new Promise((res) => setTimeout(res, 1500 + i * 1000))
        continue
      }
    } catch (e) {
      last = String(e)
    }
    await new Promise((res) => setTimeout(res, 700 * (i + 1)))
  }
  throw new Error(`siyuan POST ${path}: ${last}`)
}

async function sql<T = Record<string, unknown>>(stmt: string): Promise<T[]> {
  const d = await post<unknown>("/api/query/sql", { stmt })
  if (Array.isArray(d)) return d as T[]
  return ((d as { data?: T[] }).data ?? []) as T[]
}

// ——— ядро: встроенный MCP SiYuan (database) ———

let mcpSession = ""

/** 404 на /mcp при отправленной сессии: ядро её отклонило (рестарт ядра
 *  инвалидирует Mcp-Session-Id). Условие по статусу+заголовку, не по тексту
 *  тела ядра — формулировку может сменить апгрейд. */
class StaleMcpSessionError extends Error {}

/** Не-JSON-ответ ядра с HTTP-статусом и сниппетом тела — вместо маскирующего SyntaxError. */
function mcpHttpError(method: string, status: number, text: string): Error {
  return new Error(`mcp ${method}: HTTP ${status}: ${text.slice(0, 120)}`)
}

async function mcpRpcOnce(method: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
  const isNotification = method.startsWith("notifications/")
  const payload: Record<string, unknown> = { jsonrpc: "2.0", method, params }
  if (!isNotification) payload.id = Date.now() % 100000
  const headers: Record<string, string> = {
    Authorization: `Token ${TOKEN}`,
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
  }
  const hasSession = mcpSession !== ""
  if (hasSession) headers["Mcp-Session-Id"] = mcpSession
  const r = await fetch(SIYUAN_URL + "/mcp", { method: "POST", headers, body: JSON.stringify(payload) })
  const sid = r.headers.get("mcp-session-id")
  if (sid) mcpSession = sid
  const text = await r.text()
  if (r.status === 404 && hasSession) throw new StaleMcpSessionError(mcpHttpError(method, r.status, text).message)
  if (text.trim() === "") return {}
  const dataLines = text.split("\n").filter((l) => l.startsWith("data: "))
  const parsed = parseJsonContract(dataLines.length > 0 ? dataLines[dataLines.length - 1].slice(5) : text, mcpHttpError(method, r.status, text).message)
  const msg = parsed as { code?: unknown; msg?: unknown; jsonrpc?: unknown; error?: { code?: unknown; message?: unknown }; result?: unknown }
  // kernel-конверт ошибки поверх jsonrpc (401 «Auth failed [session]» и пр.):
  // без этой проверки молча превращался в пустой результат {}
  if (msg.jsonrpc === undefined && typeof msg.code === "number" && msg.code !== 0) {
    throw new Error(`mcp ${method}: kernel code=${msg.code} msg=${msg.msg}`)
  }
  if (msg.error) throw new Error(`mcp ${method}: ${JSON.stringify(msg.error)}`)
  return (msg.result ?? {}) as Record<string, unknown>
}

/** Re-handshake после отклонённой сессии: чистим состояние и initialize заново.
 *  reentrant-guard: вложенный отказ во время handshake — описательная ошибка, не рекурсия. */
let recovering = false

async function recoverMcpSession(): Promise<void> {
  if (recovering) throw new Error("mcp: сессия отклонена и во время re-handshake — ядро недоступно")
  recovering = true
  try {
    mcpSession = ""
    mcpReady = false
    await connect()
  } finally {
    recovering = false
  }
}

/** Один ретрай на протухшую сессию: тик лечится на месте вместо 5 падений
 *  подряд и само-рестарта демона (прод-кейс 04.11). */
async function mcpRpc(method: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
  try {
    return await mcpRpcOnce(method, params)
  } catch (e) {
    if (!(e instanceof StaleMcpSessionError)) throw e
    await recoverMcpSession()
    return mcpRpcOnce(method, params)
  }
}

export async function connect(): Promise<void> {
  if (mcpReady) return // сессия уже инициализирована (демоны зовут connect повторно)
  await mcpRpc("initialize", {
    protocolVersion: "2025-03-26",
    capabilities: {},
    clientInfo: { name: "uwuwu-daemon", version: "1.0" },
  })
  await mcpRpc("notifications/initialized", {})
  mcpReady = true
}

let mcpReady = false

async function mcpToolCall(name: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (!mcpReady) {
    await connect()
  }
  const r = await mcpRpc("tools/call", { name, arguments: args })
  if (r.isError === true) throw new Error(`tool ${name}: ${JSON.stringify(r.content)}`)
  if (r.structuredContent !== undefined) return r.structuredContent as Record<string, unknown>
  const text = (r.content as { text?: string }[] | undefined)?.[0]?.text ?? "{}"
  return parseJsonContract(text, `tool ${name}: content`) as Record<string, unknown>
}

// ——— кэш ключей database (имя → keyID) ———

const keyCache = new Map<string, Map<string, string>>()

async function avKeys(avId: string): Promise<Map<string, string>> {
  const hit = keyCache.get(avId)
  if (hit) return hit
  const r = await mcpToolCall("database", { action: "keys", id: avId })
  const keys = ((r.data as { keys?: { id?: string; name?: string }[] }) ?? {}).keys ?? []
  const m = new Map<string, string>()
  for (const k of keys) if (k.id && k.name) m.set(k.name, k.id)
  keyCache.set(avId, m)
  return m
}

// databaseBlockId (av-блок) → avID из markdown блока
async function avIdOfBlock(blockId: string): Promise<string> {
  const rows = await sql<{ markdown?: string }>(`select markdown from blocks where id='${blockId}'`)
  const md = rows[0]?.markdown ?? ""
  const m = md.match(/data-av-id="([0-9]{14}-[a-z0-9]+)"/) ?? md.match(/avID="([0-9]{14}-[a-z0-9]+)"/)
  if (m) return m[1]
  return blockId
}

// ——— эмуляция афин-тулзов ———

/** Одна страница render (page с 1; серверный pageSize — 50, см. board-view). */
async function renderPage(blockId: string, avId: string, page: number): Promise<BoardViewPage> {
  const r = await mcpToolCall("database", { action: "render", id: avId, blockID: blockId, page })
  return (r.data ?? r) as BoardViewPage
}

async function readDatabaseCells(args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const blockId = String(args.databaseBlockId ?? "")
  const avId = await avIdOfBlock(blockId)
  const view = await collectBoardView((page) => renderPage(blockId, avId, page))
  const nameOf = new Map<string, string>()
  for (const c of view.columns) if (c.id && c.name) nameOf.set(c.id, c.name)
  const rows: Record<string, unknown>[] = []
  for (const row of view.rows) {
    const cells: Record<string, { value?: string }> = {}
    let title = ""
    for (const cell of row.cells ?? []) {
      const v = cell.value ?? {}
      const col = nameOf.get(v.keyID ?? "") ?? ""
      if (v.type === "block") {
        title = v.block?.content ?? ""
        continue
      }
      let value = ""
      if (v.mSelect) value = v.mSelect.map((o) => o.content).join(",")
      else if (v.text) value = v.text.content ?? ""
      if (col !== "") cells[col] = { value }
    }
    rows.push({ rowBlockId: row.id ?? "", linkedDocId: null, title, cells })
  }
  return { rows }
}

async function updateDatabaseRow(args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const blockId = String(args.databaseBlockId ?? "")
  const avId = await avIdOfBlock(blockId)
  const rowId = String(args.rowBlockId ?? "")
  const cells = (args.cells ?? {}) as Record<string, string>
  const keys = await avKeys(avId)
  for (const [name, value] of Object.entries(cells)) {
    const keyId = keys.get(name) ?? keys.get("Status")
    if (!keyId) continue
    const typed = name === "Status" || name === "Статус"
      ? { mSelect: [{ content: String(value) }] }
      : { text: { content: String(value) } }
    await mcpToolCall("database", { action: "item_update", id: avId, keyID: keyId, itemID: rowId, value: typed })
  }
  return { updated: true }
}

function commentToSnapshot(text: string): Record<string, unknown> {
  // шейп, который walk-ает snapshotText() фабрики
  return {
    snapshot: {
      blocks: {
        id: "page", type: "block",
        children: [{ id: "note", type: "block", children: [{ id: "p", type: "block", props: { text: { delta: [{ insert: text }] } } }] }],
      },
    },
  }
}

export async function tool<T = Record<string, unknown>>(name: string, args: Record<string, unknown>): Promise<T> {
  switch (name) {
    case "find_doc_by_title": {
      const title = String(args.title ?? "")
      const rows = await sql<{ id: string }>(
        `select id from blocks where type='d' and content = '${title.replace(/'/g, "''")}' limit ${Number(args.limit ?? 5)}`)
      return { matches: rows.map((r) => ({ id: r.id, inTrash: false })) } as T
    }
    case "get_doc": {
      const docId = String(args.docId ?? "")
      const rows = await sql<{ id: string; content: string; updated: string }>(
        `select id, content, updated from blocks where type='d' and id='${docId}' limit 1`)
      const d = rows[0]
      return (d ? { id: d.id, title: d.content, updatedAt: d.updated } : { id: docId, title: "", updatedAt: "" }) as T
    }
    case "read_doc": {
      const docId = String(args.docId ?? "")
      const rows = await sql<{ id: string; type: string }>(
        `select id, type from blocks where root_id='${docId}' limit ${SQL_LIMIT}`)
      return {
        blocks: rows.map((b) => ({
          id: b.id,
          flavour: b.type === "av" ? "platform:database" : b.type === "d" ? "platform:page" : "platform:paragraph",
        })),
      } as T
    }
    case "read_database_cells":
      return readDatabaseCells(args) as T
    case "update_database_row":
      return updateDatabaseRow(args) as T
    case "export_doc_markdown": {
      const docId = String(args.docId ?? "")
      try {
        const r = await post<{ content?: string }>("/api/export/exportMdContent", { id: docId })
        const t = await sql<{ content: string }>(`select content from blocks where type='d' and id='${docId}' limit 1`)
        return { exists: true, markdown: r.content ?? "", title: t[0]?.content ?? docId } as T
      } catch {
        return { exists: false, markdown: "", title: "" } as T
      }
    }
    case "create_comment": {
      const docId = String(args.docId ?? "")
      const text = String(args.content ?? "")
      const ts = new Date().toISOString().slice(0, 16).replace("T", " ")
      await post("/api/block/appendBlock", {
        dataType: "markdown", data: `\n- **AGENTS** (${ts}): ${text}`, parentID: docId,
      })
      return { created: true } as T
    }
    case "list_comments": {
      const docId = String(args.docId ?? "")
      const md = await post<{ content?: string }>("/api/export/exportMdContent", { id: docId })
      const lines = (md.content ?? "").split("\n")
      const edges: Record<string, unknown>[] = []
      let inSection = false
      let i = 0
      for (const line of lines) {
        if (line.startsWith("## ")) { inSection = line.startsWith("## Комменты"); continue }
        if (!inSection) continue
        const top = line.match(/^- \*\*(.+?)\*\* \(([^)]*)\): (.*)$/)
        if (top) {
          edges.push({
            node: {
              id: `c${docId}-${i++}`,
              content: commentToSnapshot(`${top[1]} (${top[2]}): ${top[3]}`),
              user: { name: top[1], id: top[1] },
              replies: [],
              resolved: false,
            },
          })
          continue
        }
        const rep = line.match(/^  - \*\*(.+?)\*\* \(([^)]*)\): (.*)$/)
        if (rep && edges.length > 0) {
          const node = edges[edges.length - 1].node as { replies?: Record<string, unknown>[] }
          node.replies = node.replies ?? []
          node.replies.push({ content: commentToSnapshot(`${rep[1]} (${rep[2]}): ${rep[3]}`), user: { name: rep[1] } })
        }
      }
      return { edges, pageInfo: { hasNextPage: false, endCursor: "" } } as T
    }
    case "list_organize_nodes": {
      // Синтез организ-дерева афин-шейпа из доков SiYuan. Потребители ждут:
      // проекты — folder-узлы глубины 1 (data = тайтл), доски и доки глубже —
      // doc-узлы (data = doc id): cron.ts cronBoardTitles, factory/cron
      // boardViaOrganize, triage.ts refreshBoards (tests/platform-organize.test.ts).
      const nbs = await post<{ notebooks?: { id?: string; name?: string; closed?: boolean }[] }>(
        "/api/notebook/lsNotebooks", {})
      const nodes: Record<string, unknown>[] = []
      for (const nb of nbs.notebooks ?? []) {
        if (nb.closed || !nb.id || !nb.name) continue
        nodes.push({ id: nb.id, parentId: null, type: "folder", data: nb.name })
        const docs = await sql<{ id: string; content: string; hpath: string }>(
          `select id, content, hpath from blocks where type='d' and box='${nb.id}' limit ${SQL_LIMIT}`)
        const nodeByHpath = new Map<string, string>() // hpath → id узла-контейнера
        for (const d of docs.sort((a, b) => a.hpath.split("/").length - b.hpath.split("/").length)) {
          const parentHpath = d.hpath.split("/").slice(0, -1).join("/")
          // hpath "/proj" (2 сегмента) — глубина 1: проект-контейнер; глубже — доски/доки
          const isProjectLevel = d.hpath.split("/").length === 2
          nodes.push({
            id: d.id,
            parentId: nodeByHpath.get(parentHpath) ?? nb.id,
            type: isProjectLevel ? "folder" : "doc",
            data: isProjectLevel ? d.content : d.id,
          })
          nodeByHpath.set(d.hpath, d.id)
        }
      }
      return { nodes } as T
    }
    default:
      throw new Error(`siyuan adapter: тулза «${name}» не портирована`)
  }
}

export const __test = {
  commentToSnapshot,
  SIYUAN_URL,
  /** Живая проба одной страницы render (page с 1) — независимый источник
   *  rowCount ядра для живого теста полноты выборки. */
  renderBoardPage: async (blockId: string, page: number): Promise<BoardViewPage> => {
    const avId = await avIdOfBlock(blockId)
    return renderPage(blockId, avId, page)
  },
  // совместимость со старым тестом сессий: у SiYuan адаптера нет разделяемой
  // сессии с протуханием — expireSession безопасен как no-op
  expireSession: (): void => {
    mcpSession = ""
    mcpReady = false
  },
}
