// uwuwu-cron — демон кронов на AFFiNE.
// Поллит доску «Crons»: строка = крон (Schedule/Status/LastRun, промпт в теле).
// Наступило расписание → OpenCode-сессия с промптом из тела → агент работает
// и докладывает комментом (🤖); разовые (at) после срабатывания → paused.
// Запуск: systemd --user (uwuwu-cron.service). Тик: 1 минута.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "../factory/mcp"
import {
  due, isOneShot, isValidTracked, parseSchedule, firePrompt,
  type CronRow, type CronState,
} from "./cron-lib"

const BOARD_TITLE = "Crons"
const STATE_DIR = join(homedir(), ".local/state/uwuwu-cron")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOCK_DIR = join(STATE_DIR, "lock")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-cron.log")
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

function acquireLock(): boolean {
  mkdirSync(STATE_DIR, { recursive: true })
  try {
    mkdirSync(LOCK_DIR, { recursive: false })
  } catch {
    try {
      const pid = Number(readFileSync(join(LOCK_DIR, "pid"), "utf-8").trim())
      if (Number.isInteger(pid)) {
        process.kill(pid, 0)
        return false
      }
    } catch { /* мёртвый lock — захватываем */ }
    try {
      rmSync(LOCK_DIR, { recursive: true, force: true })
      mkdirSync(LOCK_DIR, { recursive: false })
    } catch {
      return false
    }
  }
  writeFileSync(join(LOCK_DIR, "pid"), String(process.pid))
  const release = (): void => {
    try { rmSync(LOCK_DIR, { recursive: true, force: true }) } catch {}
    process.exit(0)
  }
  process.on("SIGTERM", release)
  process.on("SIGINT", release)
  return true
}

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

async function findBoard(): Promise<BoardIds> {
  const r = await mcp.tool<{ matches?: { id: string }[] }>("find_doc_by_title", { title: BOARD_TITLE, caseInsensitive: true, limit: 1 })
  const docId = r.matches?.[0]?.id
  if (docId === undefined) throw new Error(`доска «${BOARD_TITLE}» не найдена`)
  const doc = await mcp.tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
  const dbId = doc.blocks?.find((b) => b.flavour === "affine:database" || b.flavour === "affine:data_view")?.id
  if (dbId === undefined) throw new Error(`в «${BOARD_TITLE}» нет database-блока`)
  return { docId, dbId }
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
  const board = await findBoard()
  const rows = await readCrons(board)
  const now = new Date()

  for (const row of rows) {
    if (row.status !== "active") continue
    const sched0 = parseSchedule(row.schedule)
    // reconcile: разовый уже стрелял (демон упал до setCells) → дозаповедать.
    // окно 24ч: юзерский re-arm (paused→active руками) вне окна не гасится
    const prevFire = state.crons[row.rowId]?.lastFiredAt ?? 0
    if (sched0 !== null && isOneShot(sched0) && prevFire > 0
        && now.getTime() - prevFire < 86_400_000) {
      await setCells(board, row.rowId, { Status: "paused" })
      log(`крон «${row.title || row.rowId.slice(0, 8)}»: разовый уже стрелял — paused (reconcile)`)
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

    const body = await cronBody(row.bodyDoc)
    // linked-строки несут заголовок в теле: деривируем из первой строки
    const title = row.title !== "" ? row.title
      : body.split("\n")[0]?.replace(/^#+\s*/, "").trim() || `крон ${row.rowId.slice(0, 8)}`
    const sessionID = await fireSession(title, firePrompt({ ...row, title }, body))
    if (sessionID === null) {
      log(`крон «${title}»: сессия не поднялась`)
      continue
    }
    // семантика v1: lastFiredAt = факт ухода промпта, не отчёта агента;
    // агент без 🤖-коммента невидим (детекция — следующий срез, если болит)
    state.crons[row.rowId] = { sessionID, lastFiredAt: now.getTime(), firedAtIso: now.toISOString() }
    saveState(state)
    await setCells(board, row.rowId, { "Last Run": now.toISOString().slice(0, 16).replace("T", " ") })
    if (isOneShot(sched)) {
      await setCells(board, row.rowId, { Status: "paused" })
      log(`крон «${title}»: разовый — сработал, paused`)
    }
    log(`крон «${title}»: сработал → сессия ${sessionID}`)
  }
}

async function main(): Promise<void> {
  if (!acquireLock()) {
    console.error("другой живой инстанс uwuwu-cron держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  mkdirSync(WORKSPACE, { recursive: true })
  await mcp.connect()
  log(`uwuwu-cron: старт (board=${BOARD_TITLE}, tick=${TICK_SEC}s, model=${MODEL.id})`)
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
    await new Promise((r) => setTimeout(r, TICK_SEC * 1000))
  }
}

void main()
