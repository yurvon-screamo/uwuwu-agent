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

/** Ядро не ответило на транспорте (refused/DNS/таймаут/обрыв тела ответа) —
 *  отличать от «ядро ответило ошибкой»: демоны решают по классу. Прод-кейс
 *  05.11: лежащее ядро было невидимо в логе uwuwu-cron — пер-досочные
 *  catch-скипы глотали голый TypeError, тик «успешно» проходил с пустым
 *  списком досок. Имя класса задаётся явно: рендер лога «${e.name}:
 *  ${e.message}» (cron.ts tick) иначе показывает безликое «Error:». */
export class KernelUnavailableError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "KernelUnavailableError"
  }
}

/** Ядро в волне деградации индекса: «metadata is not ready» исчерпало ретраи —
 *  это ответ ядра (code≠0 в конверте), не транспортный отказ, потому отдельный
 *  класс поверх KernelUnavailableError: семантика родителя (ADR-033, «ядро
 *  не ответило на транспорте») не расширяется, ветвление демонов
 *  instanceof KernelUnavailableError сохраняется (наследник матчится).
 *  Крон различает wave-тик точным instanceof — не считает его в
 *  consecutiveFails: окно fail-fast разрядится само, рестарт не нужен. */
export class KernelWaveError extends KernelUnavailableError {
  constructor(message: string) {
    super(message)
    this.name = "KernelWaveError"
  }
}

/** Таймаут одного kernel-вызова (REST и /mcp): зависший TCP не должен
 *  останавливать тик демона — класс «клиентские висы» (прод-контур).
 *  Валидация обязательна: Number("") = 0 дал бы AbortSignal.timeout(0) —
 *  мгновенный таймаут каждого вызова (тихая слепота контура); NaN — RangeError. */
function parseKernelTimeoutMs(raw: string | undefined): number {
  if (raw === undefined) return 15_000
  const n = Number(raw)
  if (!Number.isFinite(n) || n <= 0) {
    throw new Error(`UWUWU_KERNEL_TIMEOUT_MS: ожидалось положительное число миллисекунд, получено «${raw}»`)
  }
  return n
}

const KERNEL_TIMEOUT_MS = parseKernelTimeoutMs(process.env.UWUWU_KERNEL_TIMEOUT_MS)

// ——— ядро: wave-дисциплина (зеркало cli/src/siyuan.rs WAVE_COOLDOWN_SECS) ———

const WAVE_COOLDOWN_MS = 90_000
const WAVE_HINT =
  "ядро в волне деградации индекса: пауза 2–5 минут, затем `uwuwu-cli doctor`; при повторении — docker restart siyuan-demo. Сразу не ретраить — волна кормится ретраями"
/** Паузы ретраев metadata-волны (начальная + до двух); инъекция для тестов —
 *  мгновенный пейсинг (прецедент RowInsertPacing). */
let waveRetryBackoffsMs = [2_000, 5_000]
/** Шаг пауз прочих ошибок (транспорт/конверт), ms — 700·(попытка+1); инъекция
 *  для тестов: транспортный отказ не должен задерживать свиту на секунды. */
let otherRetryStepMs = 700
/** Окно fail-fast волны (unix-ms истечения; 0 = не армировано), per-process:
 *  после исчерпания ретраев последующие post() падают сразу, не долбя ядро. */
let waveCooldownUntil = 0

const armWaveCooldown = (): void => {
  waveCooldownUntil = Date.now() + WAVE_COOLDOWN_MS
}

const sleep = (ms: number): Promise<void> => new Promise((res) => setTimeout(res, ms))

// ——— ядро: REST ———

/** JSON.parse с честной ошибкой: HTTP-статус и сниппет тела вместо голого SyntaxError,
 *  маскирующего причину (прод-кейс 04.11: «Unexpected identifier "session"» = 404 ядра).
 *  Экспорт: тот же контракт у транспорта opencode API в cron.ts. */
export function parseJsonContract(text: string, context: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    throw new Error(`${context}: не-JSON: ${text.slice(0, 120)}`)
  }
}

async function post<T = Record<string, unknown>>(path: string, body: Record<string, unknown>, attempts = 4): Promise<T> {
  // окно волны активно: вызовы падают сразу — волна кормится ретраями,
  // клиент её не пробивает (семантика Rust post() wave-гейта)
  if (Date.now() < waveCooldownUntil) {
    throw new KernelWaveError(`siyuan POST ${path}: пропущено — ${WAVE_HINT} (окно fail-fast активно)`)
  }
  let last: string | Error = "?"
  let metadataAttempts = 0
  for (let i = 0; i < attempts; i++) {
    try {
      const r = await fetch(SIYUAN_URL + path, {
        method: "POST",
        headers: { Authorization: `Token ${TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(KERNEL_TIMEOUT_MS),
      }).catch((e: unknown) => {
        throw new KernelUnavailableError(`siyuan POST ${path}: ядро недоступно (${String(e)})`)
      })
      const text = await r.text().catch((e: unknown) => {
        // связь рвётся на середине тела ответа: тот же класс, что отказ
        // коннекта — иначе TypeError схлопнется в строку last и catch-скипы
        // демонов замаскируют деградацию ядра
        throw new KernelUnavailableError(`siyuan POST ${path}: тело ответа оборвалось (${String(e)})`)
      })
      const d = parseJsonContract(text, `siyuan POST ${path}: HTTP ${r.status}`) as { code?: number; msg?: string; data?: unknown }
      if (d.code === 0) return (d.data ?? {}) as T
      last = `code=${d.code} msg=${d.msg}`
      if (String(d.msg).includes("metadata is not ready")) {
        const backoff = waveRetryBackoffsMs[metadataAttempts]
        if (backoff !== undefined) {
          metadataAttempts++
          await sleep(backoff)
          continue
        }
        // ретраи исчерпаны: армируем окно и падаем сразу — следующий вызов
        // не долбит ядро в течение окна (терминальный throw, не в last)
        armWaveCooldown()
        throw new KernelWaveError(`siyuan POST ${path}: ${last} — ${WAVE_HINT}`)
      }
    } catch (e) {
      // wave — терминальный исход: ре-throw мимо last (иначе цикл
      // продолжил бы долбить ядро); прочий KernelUnavailableError
      // переживёт цикл — по нему демоны отличают лежащее ядро
      if (e instanceof KernelWaveError) throw e
      last = e instanceof KernelUnavailableError ? e : String(e)
    }
    await sleep(otherRetryStepMs * (i + 1))
  }
  // fetch-класс уже свёрстан как Error с контекстом — ре-throw без обёртки;
  // строковый last (code=… msg=… конверт ядра) — как раньше
  throw last instanceof Error ? last : new Error(`siyuan POST ${path}: ${last}`)
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
  let r: Response
  try {
    r = await fetch(`${SIYUAN_URL}/mcp`, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(KERNEL_TIMEOUT_MS),
    })
  } catch (e) {
    throw new KernelUnavailableError(`mcp ${method}: ядро недоступно (${String(e)})`)
  }
  const sid = r.headers.get("mcp-session-id")
  if (sid) mcpSession = sid
  const text = await r.text().catch((e: unknown) => {
    // mid-body abort: тот же класс, что отказ коннекта — catch-скипы демонов
    // (instanceof KernelUnavailableError) не должны глотать голый TypeError
    throw new KernelUnavailableError(`mcp ${method}: тело ответа оборвалось (${String(e)})`)
  })
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

/** Re-handshake после отклонённой сессии: чистим состояние и initialize
 *  заново. Single-flight: параллельные вызовы пачки словят 404 одновременно
 *  — все ждут ОДИН recovery (общий промис), лишних initialize нет; вызовы,
 *  начавшиеся во время восстановления, ретраятся после него же. */
let recovery: Promise<void> | null = null

function recoverMcpSession(): Promise<void> {
  if (recovery === null) {
    recovery = (async () => {
      mcpSession = ""
      mcpReady = false
      try {
        await connect()
      } finally {
        recovery = null
      }
    })()
  }
  return recovery
}

/** Один ретрай на протухшую сессию: тик лечится на месте вместо 5 падений
 *  подряд и само-рестарта демона (прод-кейс 04.11).
 *  recover:false — для вызовов САМОГО handshake: отказ сессии внутри
 *  recovery не должен снова звать recovery (общий промис ещё in-flight —
 *  вышло бы вечное ожидание самого себя); вместо этого описательная ошибка
 *  отвергает recovery-промис — ждущие пачки получают её, следующий тик
 *  ретраит с чистого состояния. */
async function mcpRpc(
  method: string,
  params: Record<string, unknown>,
  opts: { recover?: boolean } = {},
): Promise<Record<string, unknown>> {
  try {
    return await mcpRpcOnce(method, params)
  } catch (e) {
    if (!(e instanceof StaleMcpSessionError)) throw e
    if (opts.recover === false) {
      throw new Error(`mcp ${method}: сессия отклонена во время recovery-handshake — ядро недоступен (${e.message})`)
    }
    await recoverMcpSession()
    return mcpRpcOnce(method, params)
  }
}

/** Handshake single-flight: демоны зовут connect() повторно, а во время
 *  recovery новые вызывающие собираются в один handshake — лишних
 *  initialize-циклов на пачке параллельных вызовов нет. */
let connecting: Promise<void> | null = null

export function connect(): Promise<void> {
  if (mcpReady) return Promise.resolve() // сессия уже инициализирована
  if (connecting === null) {
    const handshake = (async () => {
      // recover:false — см. mcpRpc: вложенный отказ сессии во время
      // handshake = описательная ошибка, не рекурсивный recovery
      await mcpRpc("initialize", {
        protocolVersion: "2025-03-26",
        capabilities: {},
        clientInfo: { name: "uwuwu-daemon", version: "1.0" },
      }, { recover: false })
      await mcpRpc("notifications/initialized", {}, { recover: false })
      mcpReady = true
    })()
    // сброс слота по завершении — без вмешательства в доставку ошибки awaiter'ам
    handshake
      .finally(() => {
        if (connecting === handshake) connecting = null
      })
      .catch(() => {})
    connecting = handshake
  }
  return connecting
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
  const keys = (r.data as { keys?: { id?: string; name?: string }[] })?.keys ?? []
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
    const cells: Record<string, { value?: string; dateMs?: number | null }> = {}
    let title = ""
    // Привязка строки к док-телу: value.block.id Primary Key-ячейки — docId
    // тела (у detached-строк поля нет → null). Готча: value.blockID — это
    // id самой строки, не привязка. Извлечение безусловное — контракт CLI
    // parse_render_row (siyuan_task.rs): guard на refSubtype терял бы
    // легаси-строки, не прогнанные ядром через NormalizeBlockRefSubtype
    let linkedDocId: string | null = null
    for (const cell of row.cells ?? []) {
      const v = cell.value ?? {}
      const col = nameOf.get(v.keyID ?? "") ?? ""
      if (v.type === "block") {
        title = v.block?.content ?? ""
        linkedDocId = v.block?.id ?? null
        continue
      }
      let value = ""
      /** epoch ms date-клетки (истина для compare демона; formattedContent
       *  ядро перекрывает своим локалям-форматом — строкой сравнивать
       *  нельзя, живая проба миграции: записали «2026-11-08 02:34», ядро
       *  отдало «8 ноября 2026 г. 01:34»). */
      let dateMs: number | null = null
      if (v.mSelect) value = v.mSelect.map((o) => o.content).join(",")
      else if (v.text) value = v.text.content ?? ""
      else if (v.date) {
        dateMs = typeof v.date.content === "number" && v.date.content > 0 ? v.date.content : null
        value = v.date.formattedContent ?? ""
      }
      if (col !== "") cells[col] = { value, dateMs }
    }
    rows.push({ rowBlockId: row.id ?? "", linkedDocId, title, cells })
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
    // колонки нет → true no-op (best-effort контракт setCellsSafe): запись в
    // чужой ключ (Status) исказила бы чужую ячейку; Status-запись на доске с
    // колонкой «Статус» резолвится по русскому имени (паритет typed-чека ниже)
    const keyId = keys.get(name) ?? (name === "Status" ? keys.get("Статус") : undefined)
    if (!keyId) continue
    const typed = name === "Status" || name === "Статус"
      ? { mSelect: [{ content: String(value) }] }
      : { text: { content: String(value) } }
    await mcpToolCall("database", { action: "item_update", id: avId, keyID: keyId, itemID: rowId, value: typed })
  }
  return { updated: true }
}

export interface ParsedComment {
  id: string
  text: string
  userId: string
  replies: { id: string; text: string; userId: string }[]
}

const TOP_COMMENT = /^- \*\*(.+?)\*\* \(([^)]*)\): (.*)$/
const TOP_COMMENT_DATED = /^- \*\*(.+?)\*\* \((\d{4}-\d{2}-\d{2}[^)]*)\): (.*)$/
const REPLY_COMMENT = /^ {2}- \*\*(.+?)\*\* \(([^)]*)\): (.*)$/
const REPLY_COMMENT_DATED = /^ {2}- \*\*(.+?)\*\* \((\d{4}-\d{2}-\d{2}[^)]*)\): (.*)$/

const commentHead = (m: RegExpMatchArray): string => `${m[1]} (${m[2]}): ${m[3]}`

/** FNV-1a (32-bit) → 8 hex-символов: контентный хэш стабильных id комментов. */
const fnv1aHex = (input: string): string => {
  let hash = 0x811c9dc5
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193) >>> 0
  }
  return hash.toString(16).padStart(8, "0")
}

/** Стабильный id коммента: хэш (автор ‖ финальный текст с датой и
 *  континуациями). Позиция в доке в id не входит — снос/сдвиг чужих
 *  комментов id не меняет, lastCommentId треков не подменяется. */
const stableCommentId = (kind: "c" | "r", docId: string, userId: string, text: string): string =>
  `${kind}${docId}-${fnv1aHex(`${userId}\u0000${text}`)}`

/** Разбор коммент-записей markdown-экспорта тела. Два источника:
 *  (а) секция «## Комменты» — историческая конвенция;
 *  (б) трейлинг-хвост после последней ## — CLI аппендит комменты appendBlock'ом
 *  в конец дока, в телах с секциями после «## Комменты» (слияния дефектов —
 *  регулярный паттерн владельца) свежие записи живут хвостом; без (б) comment-
 *  канал слеп к ним — юзерский ответ не будит ни одним каналом (High-1 ревью
 *  раунда 3). Хвост строгим матчем с датой — легитимные списки не комменты.
 *  Продолжения блока (абзацы тела пункта — вердикты многострочные) конкатенируются
 *  в text — будильник цитирует коммент целиком. id двухпроходные: скан
 *  собирает финальные тексты, после скана id = контентный хэш — позиция
 *  в доке не участвует, снос старого коммента сверху id остальных не
 *  двигает (позиционные сдвигались и подменяли lastCommentId треков).
 *  Replies несут id/userId: юзерский тред-ответ детектится, агентский
 *  срезается own-фильтром. */
export function parseCommentsMarkdown(md: string, docId: string): ParsedComment[] {
  const lines = md.split("\n")
  const comments: ParsedComment[] = []
  let inSection = false
  let lastHeadIdx = -1
  let lastHeadIsComments = false
  const appendContinuation = (line: string): void => {
    if (comments.length === 0 || line.trim() === "") return
    const last = comments[comments.length - 1]
    last.text = `${last.text}\n${line}`
  }
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    if (line.startsWith("## ")) {
      lastHeadIdx = i
      lastHeadIsComments = /^## Комменты\s*$/.test(line)
      inSection = lastHeadIsComments
      continue
    }
    if (!inSection) continue
    const top = line.match(TOP_COMMENT)
    if (top) {
      comments.push({ id: "", text: commentHead(top), userId: top[1], replies: [] })
      continue
    }
    const rep = line.match(REPLY_COMMENT)
    if (rep && comments.length > 0) {
      comments[comments.length - 1].replies.push({ id: "", text: commentHead(rep), userId: rep[1] })
      continue
    }
    appendContinuation(line)
  }
  if (!lastHeadIsComments) {
    for (let i = lastHeadIdx + 1; i < lines.length; i++) {
      const line = lines[i]
      const top = line.match(TOP_COMMENT_DATED)
      if (top) {
        comments.push({ id: "", text: commentHead(top), userId: top[1], replies: [] })
        continue
      }
      const rep = line.match(REPLY_COMMENT_DATED)
      if (rep && comments.length > 0) {
        comments[comments.length - 1].replies.push({ id: "", text: commentHead(rep), userId: rep[1] })
        continue
      }
      if (i > lastHeadIdx) appendContinuation(line)
    }
  }
  for (const c of comments) {
    c.id = stableCommentId("c", docId, c.userId, c.text)
    for (const r of c.replies) r.id = stableCommentId("r", docId, r.userId, r.text)
  }
  return comments
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
      const rows = await sql<{ id: string; content: string; updated: string; box?: string; hpath?: string }>(
        `select id, content, updated, box, hpath from blocks where type='d' and id='${docId}' limit 1`)
      const d = rows[0]
      return (d
        ? { id: d.id, title: d.content, updatedAt: d.updated, box: d.box ?? "", hpath: d.hpath ?? "" }
        : { id: docId, title: "", updatedAt: "", box: "", hpath: "" }) as T
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
    case "doc_exists": {
      // Живость дока через MCP block get (не SQL): индекс blocks отстаёт от
      // свежих доков, filetree/block — FS-истина. Ядро отвечает isError
      // «was not found» за подтверждённо отсутствующий блок — это вердикт
      // «дока нет»; прочие ошибки кидаются наверх как инфра-сбой
      // (паритет CLI doc_block_exists). Известная хрупкость: матч по тексту
      // ошибки ядра — смена формулировки при апгрейде ядра даст вечный
      // infra-ретрай (крон перестаёт файрить, но и не сжигается: следить за
      // логом «тело не разрешено» после апгрейдов SiYuan).
      const docId = String(args.docId ?? "")
      const r = await mcpRpc("tools/call", { name: "block", arguments: { action: "get", id: docId } })
      const text = (r.content as { text?: string }[] | undefined)?.[0]?.text ?? ""
      if (r.isError === true) {
        if (text.includes("was not found")) return { exists: false } as T
        throw new Error(`tool block: ${JSON.stringify(r.content)}`)
      }
      return { exists: text.includes("Type: NodeDocument") } as T
    }
    case "doc_ids_by_hpath": {
      // FS-истина файлового дерева: без лага SQL-индекса (паритет CLI
      // doc_ids_by_hpath); заголовки с '/' санитайзит вызывающий (pathSafe)
      const data = await post<unknown>("/api/filetree/getIDsByHPath", {
        notebook: String(args.notebook ?? ""), path: String(args.path ?? ""),
      })
      const ids = Array.isArray(data) ? data.filter((x): x is string => typeof x === "string") : []
      return { ids } as T
    }
    case "notebook_ids_by_names": {
      // id ноутбуков по именам одним lsNotebooks (getIDsByHPath принимает
      // только id — паритет CLI siyuan_id); демонам без дока-якоря
      // (инит-блок спавна: персона Experience, доки проекта Projects).
      // Отсутствующие/закрытые имена в ответе отсутствуют.
      const wanted = (Array.isArray(args.names) ? args.names : []).map(String)
      const nbs = await post<{ notebooks?: { id?: string; name?: string; closed?: boolean }[] }>(
        "/api/notebook/lsNotebooks", {})
      const ids: Record<string, string> = {}
      for (const nb of nbs.notebooks ?? []) {
        if (nb.closed !== true && typeof nb.id === "string" && wanted.includes(nb.name ?? "")) {
          ids[nb.name as string] = nb.id
        }
      }
      return { ids } as T
    }
    case "export_doc_markdown": {
      const docId = String(args.docId ?? "")
      try {
        const r = await post<{ content?: string }>("/api/export/exportMdContent", { id: docId })
        const t = await sql<{ content: string }>(`select content from blocks where type='d' and id='${docId}' limit 1`)
        return { exists: true, markdown: r.content ?? "", title: t[0]?.content ?? docId } as T
      } catch (e) {
        // ADR-033: лежащее ядро — не «дока нет»: транспортный класс проходит
        // транзитом; {exists:false} — только сбой/отсутствие при живом ядре
        if (e instanceof KernelUnavailableError) throw e
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
    case "get_doc_attrs": {
      // Атрибуты блока (IAL): custom-* — носитель расписания крон-задач.
      // REST, не SQL-колонка ial: та отстаёт от свежих записей атрибутов.
      const docId = String(args.docId ?? "")
      const data = await post<Record<string, unknown>>("/api/attr/getBlockAttrs", { id: docId })
      const out: Record<string, string> = {}
      for (const [k, v] of Object.entries(data)) {
        if (typeof v === "string") out[k] = v
      }
      return out as T
    }
    case "set_doc_attr": {
      // Запись атрибута; пустое значение снимает (ядро удаляет ключ из IAL)
      const docId = String(args.docId ?? "")
      const name = String(args.name ?? "")
      const value = String(args.value ?? "")
      await post("/api/attr/setBlockAttrs", { id: docId, attrs: { [name]: value } })
      return { set: true } as T
    }
    case "list_comments": {
      const docId = String(args.docId ?? "")
      const md = await post<{ content?: string }>("/api/export/exportMdContent", { id: docId })
      const edges: Record<string, unknown>[] = []
      for (const c of parseCommentsMarkdown(md.content ?? "", docId)) {
        edges.push({
          node: {
            id: c.id,
            content: commentToSnapshot(c.text),
            user: { name: c.userId, id: c.userId },
            replies: c.replies.map((r) => ({
              id: r.id,
              content: commentToSnapshot(r.text),
              user: { name: r.userId, id: r.userId },
            })),
            resolved: false,
          },
        })
      }
      return { edges, pageInfo: { hasNextPage: false, endCursor: "" } } as T
    }
    case "list_organize_nodes": {
      // Синтез организ-дерева афин-шейпа из доков SiYuan. Потребители ждут:
      // проекты — folder-узлы глубины 1 (data = тайтл), доски и доки глубже —
      // doc-узлы (data = doc id): cron.ts taskBoardTitles, factory/cron
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
  parseKernelTimeoutMs,
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
  /** Инъекции wave-тестов: мгновенный пейсинг ретраев и сброс окна
   *  между тестами (module-level состояние изолируется руками). */
  setWaveRetryBackoffsMs: (backoffs: number[]): void => {
    waveRetryBackoffsMs = backoffs
  },
  setOtherRetryStepMs: (stepMs: number): void => {
    otherRetryStepMs = stepMs
  },
  resetWaveCooldown: (): void => {
    waveCooldownUntil = 0
  },
}
