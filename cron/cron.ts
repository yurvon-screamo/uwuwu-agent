// uwuwu-cron — демон кронов (SiYuan).
// Поллит все крон-доски: глобальную «Crons» + «Кроны <Proj>» по папкам Projects
// (собираются динамически, отсутствующие доски тихо скипаются): строка = крон
// (Schedule/Status/Last Run, промпт в теле). Наступило расписание → OpenCode-сессия
// с промптом из тела; здоровый тик тихий (факт — ячейка Last Run, коммент агента —
// только исключение); разовые (at) после срабатывания → fired (paused — только
// явное решение юзера).
// Запуск: systemd --user (uwuwu-cron.service). Тик: 1 минута.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, writeFileSync, renameSync, existsSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "../lib/platform"
import { acquireLock, makeDrain } from "../lib/single-instance"
import {
  due, fireGate, isOneShot, isValidTracked, parseSchedule, firePrompt,
  type CronRow, type CronState,
} from "./cron-lib"

const STATE_DIR = join(homedir(), ".local/state/uwuwu-cron")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-cron.log")

/** Локальное время «YYYY-MM-DD HH:MM» — Last Run на доске должен читаться юзером, не UTC. */
function formatLocal(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
/** изолированный cwd крон-агентов: тело крона редактируемо удалённо —
 *  исполняющая среда не должна видеть весь домашний каталог */
const WORKSPACE = join(homedir(), ".local/share/uwuwu-cron/workspace")
const TICK_SEC = Number(process.env.UWUWU_CRON_TICK_SEC ?? 60)
const ONCE = process.env.UWUWU_CRON_ONCE === "1"
/** пауза между срабатываниями одного крона пока жива его сессия */
const SESSION_COOLDOWN_MS = 10 * 60_000
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
}

// ——— одиночный инстанс ———

// onStop лока взводит флаг: тик доезжает до конца, новых сессий не спавним
const drain = makeDrain()

// ——— состояние ———

function loadState(): CronState {
  try {
    const raw: unknown = JSON.parse(readFileSync(STATE_FILE, "utf-8"))
    const crons = (raw as { crons?: unknown }).crons
    const out: CronState = { crons: {} }
    if (crons !== null && typeof crons === "object") {
      for (const [k, v] of Object.entries(crons as Record<string, unknown>)) {
        if (isValidTracked(v)) out.crons[k] = v
        else log(`state: запись ${k.slice(0, 8)} битая — пропущена`)
      }
    }
    return out
  } catch {
    return { crons: {} }
  }
}

function saveState(state: CronState): void {
  mkdirSync(STATE_DIR, { recursive: true })
  if (existsSync(STATE_FILE)) {
    try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
  }
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 2))
  renameSync(tmp, STATE_FILE)
}

// ——— доска ———

interface BoardIds { docId: string; dbId: string }

/** Крон-доски: глобальная «Crons» + «Кроны <proj>» по папкам Projects
 *  (динамически — новые проекты подхватываются без правки демона).
 *  Отсутствующие доски тихо скипаются (создаёт юзер руками). */
const GLOBAL_CRON_BOARD = "Crons"

async function cronBoardTitles(): Promise<string[]> {
  try {
    const r = await mcp.tool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>(
      "list_organize_nodes", {})
    const nodes = r.nodes ?? []
    const pid = nodes.find(n => n.type === "folder" && n.data === "Projects")?.id
    const projects = nodes
      .filter(n => n.type === "folder" && n.parentId === pid)
      .map(n => n.data)
      .filter((d): d is string => typeof d === "string")
    return [GLOBAL_CRON_BOARD, ...projects.map(p => `Кроны ${p}`)]
  } catch { return [GLOBAL_CRON_BOARD] }
}

async function findBoard(title: string): Promise<BoardIds | null> {
  const r = await mcp.tool<{ matches?: { id: string; inTrash?: boolean }[] }>("find_doc_by_title", { title, caseInsensitive: true, limit: 5 })
  let docId = r.matches?.find(m => m.inTrash !== true)?.id
  if (docId === undefined) {
    // индекс тайтлов отстаёт от свежих/переименованных доков — фоллбек
    // через организ-дерево: «Кроны X» → папка X → дети по точному тайтлу
    const proj = title.startsWith("Кроны ") ? title.slice("Кроны ".length) : null
    if (proj !== null) docId = await boardViaOrganize(proj, title) ?? undefined
  }
  if (docId === undefined) return null
  const doc = await mcp.tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
  const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id
  if (dbId === undefined) return null
  return { docId, dbId }
}

/** Доска без поискового индекса: «Кроны <proj>» → папка <proj> → доки. */
async function boardViaOrganize(proj: string, title: string): Promise<string | null> {
  try {
    const r = await mcp.tool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>(
      "list_organize_nodes", {})
    const nodes = r.nodes ?? []
    const folders = nodes.filter(n => n.type === "folder" && n.data === proj)
    for (const f of folders) {
      const docIds = nodes
        .filter(n => n.type === "doc" && n.parentId === f.id)
        .map(n => n.data)
        .filter((d): d is string => typeof d === "string")
      for (const docId of docIds) {
        const d = await mcp.tool<{ title?: string }>("get_doc", { docId })
        if (d.title === title) return docId
      }
    }
  } catch { /* организ недоступен — остаётся null */ }
  return null
}

async function findCronBoards(): Promise<{ title: string; board: BoardIds }[]> {
  const out: { title: string; board: BoardIds }[] = []
  for (const title of await cronBoardTitles()) {
    try {
      const board = await findBoard(title)
      if (board !== null) out.push({ title, board })
    } catch { /* доска недоступна — скип тика по ней */ }
  }
  return out
}

async function readCrons(board: BoardIds): Promise<CronRow[]> {
  const r = await mcp.tool<{
    rows?: {
      rowBlockId?: string
      linkedDocId?: string | null
      title?: string | null
      cells?: Record<string, { value?: string }>
    }[]
  }>("read_database_cells", { docId: board.docId, databaseBlockId: board.dbId })
  return (r.rows ?? [])
    .filter((row) => typeof row.rowBlockId === "string")
    .map((row) => ({
      rowId: row.rowBlockId as string,
      bodyDoc: row.linkedDocId ?? "",
      title: (row.title ?? "").trim(),
      schedule: row.cells?.Schedule?.value?.trim() ?? "",
      status: (row.cells?.Status?.value ?? "").toLowerCase(),
      lastRun: row.cells?.["Last Run"]?.value ?? "",
    }))
}

async function setCells(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<void> {
  await mcp.tool("update_database_row", { docId: board.docId, databaseBlockId: board.dbId, rowBlockId: rowId, cells })
}

async function cronBody(bodyDoc: string): Promise<string> {
  if (bodyDoc === "") return ""
  const r = await mcp.tool<{ markdown?: string }>("export_doc_markdown", { docId: bodyDoc })
  return r.markdown ?? ""
}

// ——— opencode REST ———

async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = "Basic " + Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")
  const res = await fetch(`${base}/api${path}`, {
    method: init?.method ?? "GET",
    headers: { authorization: auth, "content-type": "application/json" },
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
    signal: AbortSignal.timeout(15_000),
  })
  const text = await res.text()
  if (!res.ok) throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
  if (text.trim() === "") return {}
  const j: unknown = JSON.parse(text)
  return (j as { data?: unknown }).data ?? j
}

async function fireSession(title: string, prompt: string): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: `⏰ ${title.replace(/[\r\n]+/g, " ").slice(0, 60)}`,
      model: { providerID: MODEL.providerID, id: MODEL.id },
      location: { directory: WORKSPACE },
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  if (typeof id !== "string" || !id.startsWith("ses_")) return null
  const ok = await restCall(`/session/${id}/prompt`, { method: "POST", body: { text: prompt } }).then(() => true).catch(() => false)
  if (!ok) {
    // rollback: сессия без промпта — сирота
    await restCall(`/session/${id}`, { method: "DELETE" }).catch(() => {})
    return null
  }
  return id
}

async function sessionAlive(sessionID: string): Promise<boolean | null> {
  try {
    await restCall(`/session/${sessionID}`)
    return true
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) return false
    return null // неизвестно — не блокируем
  }
}

// ——— тик ———

async function tick(state: CronState): Promise<void> {
  const boards = await findCronBoards()
  const now = new Date()
  for (const { title: boardTitle, board } of boards) {
    await tickBoard(state, board, boardTitle, now)
  }
}

async function tickBoard(state: CronState, board: BoardIds, boardTitle: string, now: Date): Promise<void> {
  const rows = await readCrons(board)

  for (const row of rows) {
    if (row.status !== "active") continue
    const sched0 = parseSchedule(row.schedule)
    // reconcile: разовый уже стрелял (демон упал до setCells) → дозаповедать.
    // окно 24ч: юзерский re-arm (paused→active руками) вне окна не гасится
    const prevFire = state.crons[row.rowId]?.lastFiredAt ?? 0
    if (sched0 !== null && isOneShot(sched0) && prevFire > 0
        && now.getTime() - prevFire < 86_400_000) {
      await setCells(board, row.rowId, { Status: "fired" })
      log(`крон «${row.title || row.rowId.slice(0, 8)}»: разовый уже стрелял — fired (reconcile)`)
      continue
    }
    const sched = sched0
    if (sched === null) {
      // диагностика: молчащий навсегда крон — худший режим отказа
      log(`крон «${row.title || row.rowId.slice(0, 8)}»: кривое расписание «${row.schedule}» — пропущен`)
      continue
    }

    const tracked = state.crons[row.rowId]
    const lastFired = tracked?.lastFiredAt ?? 0

    // предыдущая сессия ещё жива → не спамим (кроме давно повисших)
    if (tracked !== undefined && tracked.sessionID !== "") {
      const alive = await sessionAlive(tracked.sessionID)
      if (alive === true && now.getTime() - lastFired < SESSION_COOLDOWN_MS) continue
      if (alive === null) continue
    }

    if (!due(sched, lastFired, now)) continue
    // стоп-сигнал: не спавним новую работу — крон доедет при следующем старте
    if (drain.isStopping()) break

    const body = await cronBody(row.bodyDoc)
    // linked-строки несут заголовок в теле: деривируем из первой строки
    const title = row.title !== "" ? row.title
      : body.split("\n")[0]?.replace(/^#+\s*/, "").trim() || `крон ${row.rowId.slice(0, 8)}`
    // проект из тайтла доски («Кроны <proj>»): явный --project в дологе —
    // агенту не нужно угадывать (глобальная доска «Crons» не используется)
    const project = boardTitle.startsWith("Кроны ") ? boardTitle.slice("Кроны ".length) : "uwuwu"
    // срез доски прочитан в начале тика — перед спавном строка перепроверяется
    const gate = fireGate(await readCrons(board), row.rowId)
    if (gate !== "fire") {
      log(`крон «${title}»: ${gate === "removed" ? "строка снята" : "строка паузнута"} — пропуск`)
      continue
    }
    const sessionID = await fireSession(title, firePrompt({ ...row, title }, body, project))
    if (sessionID === null) {
      log(`крон «${title}»: сессия не поднялась`)
      continue
    }
    // семантика v1: lastFiredAt = факт ухода промпта, не отчёта агента;
    // агент без 🤖-коммента невидим (детекция — следующий срез, если болит)
    state.crons[row.rowId] = { sessionID, lastFiredAt: now.getTime(), firedAtIso: now.toISOString() }
    saveState(state)
    await setCells(board, row.rowId, { "Last Run": formatLocal(now) })
    if (isOneShot(sched)) {
      // fired ≠ paused: разовый сработал и завершён (самообъясняюще из
      // таблицы); paused — только явное решение юзера остановить повторяющийся
      await setCells(board, row.rowId, { Status: "fired" })
      log(`крон «${title}»: разовый — сработал, fired`)
    }
    log(`крон «${title}»: сработал → сессия ${sessionID}`)
  }
}

async function main(): Promise<void> {
  // env шлюза обязателен на старте (раньше это делал process.exit на импорте
  // mcp.ts — теперь проверка явная, модуль шарится с ленивой конфигурацией)
  if (!mcp.mcpConfigured()) {
    console.error("mcp: нужен UWUWU_SIYUAN_TOKEN (или UWUWU_SIYUAN_WS)")
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: drain.stop })) {
    console.error("другой живой инстанс uwuwu-cron держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  mkdirSync(WORKSPACE, { recursive: true })
  await mcp.connect()
  log(`uwuwu-cron: старт (глобальная + кроны проектов, tick=${TICK_SEC}s, model=${MODEL.id})`)
  const state = loadState()
  log(`кронов в state: ${Object.keys(state.crons).length}`)
  let consecutiveFails = 0
  while (true) {
    try {
      await tick(state)
      consecutiveFails = 0
    } catch (e) {
      consecutiveFails++
      log(`tick (${consecutiveFails} подряд): ${e}`)
      if (consecutiveFails >= 5) {
        // отравленное соединение/пул: свежий процесс чище вечных ретраев
        log("5 тиков подряд упали — рестартуюсь (systemd поднимет)")
        process.exit(1)
      }
      await mcp.connect().catch(() => {}) // попробовать пересобрать сессию сразу
    }
    if (ONCE) {
      log("ONCE-режим: тик выполнен — выходим")
      process.exit(0)
    }
    if (drain.isStopping()) {
      log("стоп-сигнал: тик доехал — выходим")
      process.exit(0)
    }
    await drain.sleep(TICK_SEC * 1000)
  }
}

void main()
