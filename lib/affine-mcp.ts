// lib/affine-mcp.ts — адаптер платформы.
//
// С 04.11.2026 бекенд — SiYuan (катовор с AFFiNE). Файл сохраняет экспортный
// контракт (mcpConfigured/connect/tool), но разговаривает с SiYuan kernel
// (REST) и его встроенным MCP (database-операции). Шейпы ответов эмулируют
// прежние афин-тулзы, чтобы cron/factory/triage работали без правок.
//
// Env: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806), UWUWU_SIYUAN_TOKEN
// (деф. api.token из $UWUWU_SIYUAN_WS/conf/conf.json).

import { readFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const SIYUAN_URL = process.env.UWUWU_SIYUAN_URL || "http://127.0.0.1:6806"

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

async function post<T = Record<string, unknown>>(path: string, body: Record<string, unknown>, attempts = 4): Promise<T> {
  let last = "?"
  for (let i = 0; i < attempts; i++) {
    try {
      const r = await fetch(SIYUAN_URL + path, {
        method: "POST",
        headers: { Authorization: `Token ${TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify(body),
      })
      const d = (await r.json()) as { code?: number; msg?: string; data?: unknown }
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

async function mcpRpc(method: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
  const isNotification = method.startsWith("notifications/")
  const payload: Record<string, unknown> = { jsonrpc: "2.0", method, params }
  if (!isNotification) payload.id = Date.now() % 100000
  const headers: Record<string, string> = {
    Authorization: `Token ${TOKEN}`,
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
  }
  if (mcpSession !== "") headers["Mcp-Session-Id"] = mcpSession
  const r = await fetch(SIYUAN_URL + "/mcp", { method: "POST", headers, body: JSON.stringify(payload) })
  const sid = r.headers.get("mcp-session-id")
  if (sid) mcpSession = sid
  const text = await r.text()
  if (text.trim() === "") return {}
  const dataLines = text.split("\n").filter((l) => l.startsWith("data: "))
  const parsed = JSON.parse(dataLines.length > 0 ? dataLines[dataLines.length - 1].slice(5) : text)
  if (parsed.error) throw new Error(`mcp ${method}: ${JSON.stringify(parsed.error)}`)
  return (parsed.result ?? {}) as Record<string, unknown>
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
  return JSON.parse(text) as Record<string, unknown>
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

interface RenderRow {
  id?: string
  cells?: { value?: { keyID?: string; type?: string; block?: { content?: string }; mSelect?: { content?: string }[]; text?: { content?: string } } }[]
}

async function readDatabaseCells(args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const blockId = String(args.databaseBlockId ?? "")
  const avId = await avIdOfBlock(blockId)
  const r = await mcpToolCall("database", { action: "render", id: avId, blockID: blockId })
  const data = (r.data ?? r) as {
    view?: { columns?: { id?: string; name?: string }[]; rows?: RenderRow[] }
  }
  const columns = data.view?.columns ?? []
  const nameOf = new Map<string, string>()
  for (const c of columns) if (c.id && c.name) nameOf.set(c.id, c.name)
  const rows: Record<string, unknown>[] = []
  for (const row of data.view?.rows ?? []) {
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
        `select id, type from blocks where root_id='${docId}'`)
      return {
        blocks: rows.map((b) => ({
          id: b.id,
          flavour: b.type === "av" ? "affine:database" : b.type === "d" ? "affine:page" : "affine:paragraph",
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
      // синтез организ-дерева из ноутбука Projects (folder = проект, docs = дети)
      const nbs = await post<{ notebooks?: { id?: string; name?: string; closed?: boolean }[] }>(
        "/api/notebook/lsNotebooks", {})
      const nodes: Record<string, unknown>[] = []
      for (const nb of nbs.notebooks ?? []) {
        if (nb.closed || !nb.id || !nb.name) continue
        nodes.push({ id: nb.id, parentId: null, type: "folder", data: nb.name })
        const docs = await sql<{ id: string; content: string; hpath: string }>(
          `select id, content, hpath from blocks where type='d' and box='${nb.id}'`)
        const byHpath = new Map<string, string>() // hpath → folderNodeId (doc id)
        for (const d of docs.sort((a, b) => a.hpath.split("/").length - b.hpath.split("/").length)) {
          const parent = d.hpath.split("/").slice(0, -1).join("/")
          nodes.push({ id: d.id, parentId: byHpath.get(parent) ?? nb.id, type: "doc", data: d.content })
          byHpath.set(d.hpath, d.id)
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
  // совместимость со старым тестом сессий: у SiYuan адаптера нет разделяемой
  // сессии с протуханием — expireSession безопасен как no-op
  expireSession: (): void => {
    mcpSession = ""
    mcpReady = false
  },
}
