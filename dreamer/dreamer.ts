// uwuwu-dreamer — демон агентной пост-обработки сессий.
// Поллит opencode.db (readonly): уснувшая корневая сессия → дочерняя сессия
// опенкода с агентом dreamer (glm-5.3#max), который автономно обновляет вики
// (гигиена доков, опыт, инфра-задачи). Замена механизма заявок и qwen-фактизации.
// Запуск: systemd --user (линукс) или `bun dreamer.ts` (вин/ручной). Тик: 1 мин.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { Database } from "bun:sqlite"
import {
  applyRunOutcome, dreamerPrompt, emptyState, fetchRootSessions, fetchSessionById,
  hasAssistantReply, hasAssistantReplySince, idleBefore, markBacklog, normalizeTracked,
  pickCandidates, wakeupPrompt,
  type DreamerState, type TrackedSession,
} from "./dreamer-lib"

const STATE_DIR = join(homedir(), ".local/state/uwuwu-dreamer")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOCK_DIR = join(STATE_DIR, "lock")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-dreamer.log")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

const DB_PATH = process.env.UWUWU_DREAMER_DB
  ?? join(homedir(), ".local/share/opencode/opencode.db")
// тулзы живут в этом же репо (демон — dreamer/, аудит — tools/)
const AUDIT_PATH = process.env.UWUWU_DREAMER_AUDIT
  ?? resolve(import.meta.dir, "../tools/opencode_audit.py")
const WIKI_OPS_PATH = process.env.UWUWU_DREAMER_WIKIOPS
  ?? resolve(import.meta.dir, "../tools/wiki_ops.py")

const TICK_SEC = Number(process.env.UWUWU_DREAMER_TICK_SEC ?? 60)
const ONCE = process.env.UWUWU_DREAMER_ONCE === "1"
const DRY_RUN = process.env.UWUWU_DREAMER_DRY_RUN === "1"
/** сессия считается завершённой, если молчит дольше этого окна */
const IDLE_MS = Number(process.env.UWUWU_DREAMER_IDLE_MIN ?? 30) * 60_000
/** первый старт: сессии, уснувшие раньше cutoff, размечаются skipped */
const BACKLOG_DAYS = Number(process.env.UWUWU_DREAMER_BACKLOG_DAYS ?? 3)
/** дример живёт не дольше — потом failed → retry */
const RUN_TIMEOUT_MS = Number(process.env.UWUWU_DREAMER_RUN_TIMEOUT_MIN ?? 120) * 60_000
/** после спавна прогон нельзя закрыть без ответа раньше этого окна
 *  (гонка «спавн → появление сессии в /api/session/active») */
const GRACE_MS = 3 * 60_000
const MAX_ATTEMPTS = 3
/** одновременных дримеров: throttle инфраструктуры, не фильтр значимости */
const CONCURRENCY = Number(process.env.UWUWU_DREAMER_CONCURRENCY ?? 1)

const AGENT_ID = "dreamer"
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3", variant: "max" } as const

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

function loadState(): DreamerState {
  try {
    const raw: unknown = JSON.parse(readFileSync(STATE_FILE, "utf-8"))
    const sessions = (raw as { sessions?: unknown }).sessions
    const out = emptyState()
    out.bootstrapped = (raw as { bootstrapped?: unknown }).bootstrapped === true
    if (sessions !== null && typeof sessions === "object") {
      for (const [k, v] of Object.entries(sessions as Record<string, unknown>)) {
        const normalized = normalizeTracked(v as Record<string, unknown>)
        if (normalized !== null) out.sessions[k] = normalized
        else log(`state: запись ${k.slice(0, 8)} битая — пропущена`)
      }
    }
    return out
  } catch {
    return emptyState()
  }
}

function saveState(state: DreamerState): void {
  mkdirSync(STATE_DIR, { recursive: true })
  if (existsSync(STATE_FILE)) {
    try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
  }
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 2))
  renameSync(tmp, STATE_FILE)
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

/** Спавн дочерней сессии дримера: parentID наследует локацию (cwd проекта)
 *  и связывает пары «сессия ↔ её анализ» в дереве сессий. */
async function fireDreamer(sid: string, title: string, prompt: string): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      parentID: sid,
      agent: AGENT_ID,
      model: { providerID: MODEL.providerID, id: MODEL.id, variant: MODEL.variant },
      title: `💭 ${title.replace(/[\r\n]+/g, " ").slice(0, 60)}`,
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

/** Завершённость = сессии НЕТ в /api/session/active (её drain не работает).
 *  Проверено на живой ноде: завершённый дример отсутствует в active,
 *  работающая сессия присутствует. GET /session/{id} для этого непригоден —
 *  он отвечает 200 и для давно завершённых сессий. null = сеть/сервис
 *  недоступны — решение откладываем. */
async function sessionFinished(sid: string): Promise<boolean | null> {
  try {
    const raw = await restCall("/session/active")
    const data = raw !== null && typeof raw === "object" ? (raw as Record<string, unknown>) : {}
    return !(sid in data)
  } catch {
    return null
  }
}

/** Живость по GET /session/{id}: true = существует (не 404/400).
 *  Для wakeup-верификации: завершённая idle-сессия тоже «жива» — будить можно. */
async function sessionAlive(sessionID: string): Promise<boolean | null> {
  try {
    await restCall(`/session/${sessionID}`)
    return true
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) return false
    return null
  }
}

/** time_updated сессии дримера: живая, но молчащая дольше таймаута = зависла. */
function dreamerTimedOut(db: Database, dreamerSes: string): boolean {
  const row = db
    .query(`SELECT time_updated FROM session_v2 WHERE id = ?`)
    .get(dreamerSes) as { time_updated: number } | null
  if (row === null) return false // сессии нет в БД — прогон закрыт реконсилиацией
  return Date.now() - row.time_updated > RUN_TIMEOUT_MS
}

// ——— тик ———

/** Открыть новый прогон: snapshot time_updated фиксирует «оживание» далее. */
function startRun(
  state: DreamerState,
  sid: string,
  timeUpdated: number,
  attempts: number,
): TrackedSession {
  const tracked: TrackedSession = {
    status: "pending",
    lastProcessedUpdate: timeUpdated,
    attempts,
    dreamerSes: null,
    wokenAt: 0,
    at: Date.now(),
  }
  state.sessions[sid] = tracked
  return tracked
}

async function tick(state: DreamerState, db: Database): Promise<void> {
  const now = Date.now()
  const sessions = fetchRootSessions(db, idleBefore(now, IDLE_MS))

  if (!state.bootstrapped) {
    const cutoff = now - BACKLOG_DAYS * 86_400_000
    const marked = markBacklog(db, state, cutoff)
    state.bootstrapped = true
    log(`первый старт: backlog-катофф ${BACKLOG_DAYS}д — ${marked} сессий skipped`)
    saveState(state)
  }

  // ——— гигиена инварианта «≤1 дример-сессия на исходную»: исходник удалён
  //     из опенкода при любом статусе → гасим осиротевшего дримера —
  //     иначе он навсегда повиснет в опенкоде без потребителя ———
  for (const [sid, tracked] of Object.entries(state.sessions)) {
    if (tracked.dreamerSes === null || tracked.status === "running") continue
    if (fetchSessionById(db, sid) === null) {
      await restCall(`/session/${tracked.dreamerSes}`, { method: "DELETE" }).catch(() => {})
      tracked.dreamerSes = null
      tracked.at = Date.now()
      log(`${sid.slice(0, 8)}: исходная сессия удалена — дример-сессия погашена`)
      saveState(state)
    }
  }

  // ——— реконсилиация живых прогонов (не зависит от idle-списка: оживание
  //     анализируемой сессии не должно мешать закрыть прогон по факту) ———
  const running = Object.entries(state.sessions)
    .filter(([, t]) => t.status === "running")
  for (const [sid, tracked] of running) {
    if (tracked.dreamerSes === null) continue
    // young-run grace: между спавном/wakeup и попаданием сессии в /active
    // есть окно — не закрываем прогон без ответа раньше GRACE_MS
    if (Date.now() - tracked.at < GRACE_MS) continue
    const session = fetchSessionById(db, sid)
    if (session === null) {
      // исходная сессия удалена: анализировать нечего — гасим дримера и
      // закрываем прогон, иначе слот зависнет навсегда (concurrency=1 → стоп очереди)
      await restCall(`/session/${tracked.dreamerSes}`, { method: "DELETE" }).catch(() => {})
      tracked.dreamerSes = null
      tracked.status = "done"
      tracked.at = Date.now()
      log(`прогон ${sid.slice(0, 8)}: исходная сессия удалена — прогон закрыт`)
      saveState(state)
      continue
    }
    const finished = await sessionFinished(tracked.dreamerSes)
    if (finished === null) continue
    const timedOut = !finished && dreamerTimedOut(db, tracked.dreamerSes)
    if (!finished && !timedOut) continue
    // wakeup-прогон: ответ должен быть ПОЗЖЕ пробуждения (старые — прошлый цикл)
    const replied = tracked.wokenAt > 0
      ? hasAssistantReplySince(db, tracked.dreamerSes, tracked.wokenAt)
      : hasAssistantReply(db, tracked.dreamerSes)
    const result = applyRunOutcome(tracked, session, { replied, dead: finished, timedOut }, MAX_ATTEMPTS)
    if (result === "failed" || result === "gave_up") {
      // зависший дример может доплывать и писать в вики вне контроля state —
      // гасим его (dead-случаи безвредны: 404 → catch)
      await restCall(`/session/${tracked.dreamerSes}`, { method: "DELETE" }).catch(() => {})
      tracked.dreamerSes = null
    }
    log(`прогон ${sid.slice(0, 8)} → ${result} (replied=${replied}${timedOut ? ", timeout" : ""})`)
    saveState(state)
  }

  // ——— кандидаты: wakeup существующей дример-сессии или спавн новой ———
  const candidates = pickCandidates(sessions, state, MAX_ATTEMPTS)
  const activeRuns = (): number =>
    Object.values(state.sessions).filter((t) => t.status === "running" || t.status === "pending").length
  log(`очередь: ${candidates.length} кандидатов, активных прогонов: ${activeRuns()}`)
  for (const cand of candidates) {
    if (activeRuns() >= CONCURRENCY) break
    if (DRY_RUN) {
      log(`DRY_RUN: ${cand.action} дримера над ${cand.session.id.slice(0, 8)} (${cand.reason}) — пропущен`)
      continue
    }
    const prev = state.sessions[cand.session.id]

    if (cand.action === "wakeup" && prev !== undefined && prev.dreamerSes !== null) {
      const dreamerSes = prev.dreamerSes
      // верификация живости: либа детерминирована от state, REST — здесь
      const alive = await sessionAlive(dreamerSes)
      if (alive === true) {
        // атомарно: running+wokenAt ДО промпта (краш между ними → реконсилиация
        // закроет как failed+attempt, восстановления без потерь)
        prev.status = "running"
        prev.wokenAt = Date.now()
        prev.at = prev.wokenAt
        if (cand.resetAttempts) prev.attempts = 0
        saveState(state)
        const prompt = wakeupPrompt({
          sessionId: cand.session.id,
          sessionTitle: cand.session.title,
          auditPath: AUDIT_PATH,
          wikiOpsPath: WIKI_OPS_PATH,
          dbPath: DB_PATH,
          sinceIso: new Date(prev.lastProcessedUpdate).toISOString(),
        })
        const ok = await restCall(`/session/${dreamerSes}/prompt`, { method: "POST", body: { text: prompt } }).then(() => true).catch(() => false)
        if (!ok) {
          prev.attempts += 1
          prev.status = prev.attempts >= MAX_ATTEMPTS ? "gave_up" : "failed"
          prev.at = Date.now()
          log(`сессия ${cand.session.id.slice(0, 8)} (${cand.reason}): wakeup-промпт упал (попытка ${prev.attempts})`)
          saveState(state)
          continue
        }
        log(`сессия ${cand.session.id.slice(0, 8)} (${cand.reason}): wakeup дримера ${dreamerSes}`)
        continue
      }
      if (alive === null) continue // неизвестно — не решаем
      // 404/400: дример-сессия удалена → spawn-fallback (инвариант ≤1 живого)
      log(`сессия ${cand.session.id.slice(0, 8)} (${cand.reason}): дример-сессия ${dreamerSes.slice(0, 8)} мертва — пересоздание`)
      prev.dreamerSes = null
    }

    const prompt = dreamerPrompt({
      sessionId: cand.session.id,
      sessionTitle: cand.session.title,
      auditPath: AUDIT_PATH,
      wikiOpsPath: WIKI_OPS_PATH,
      dbPath: DB_PATH,
    })
    const tracked = startRun(state, cand.session.id, cand.session.timeUpdated, cand.resetAttempts ? 0 : (prev?.attempts ?? 0))
    const dreamerSes = await fireDreamer(cand.session.id, cand.session.title ?? "сессия", prompt)
    if (dreamerSes === null) {
      // спавн не удался: это тоже попытка — иначе вечный цикл
      tracked.attempts += 1
      tracked.status = tracked.attempts >= MAX_ATTEMPTS ? "gave_up" : "failed"
      tracked.at = Date.now()
      log(`сессия ${cand.session.id.slice(0, 8)} (${cand.reason}): дример не поднялся (попытка ${tracked.attempts})`)
      saveState(state)
      continue
    }
    tracked.status = "running"
    tracked.dreamerSes = dreamerSes
    tracked.at = Date.now()
    log(`сессия ${cand.session.id.slice(0, 8)} (${cand.reason}): дример ${dreamerSes}`)
    saveState(state)
  }
}

async function main(): Promise<void> {
  if (!acquireLock()) {
    console.error("другой живой инстанс uwuwu-dreamer держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  log(`uwuwu-dreamer: старт (tick=${TICK_SEC}s, idle=${IDLE_MS / 60_000}м, model=${MODEL.id}#${MODEL.variant}, db=${DB_PATH}${DRY_RUN ? ", DRY_RUN" : ""})`)
  const state = loadState()
  log(`сессий в state: ${Object.keys(state.sessions).length}`)
  let consecutiveFails = 0
  while (true) {
    let db: Database | undefined
    try {
      db = new Database(DB_PATH, { readonly: true })
      await tick(state, db)
      consecutiveFails = 0
    } catch (e) {
      consecutiveFails++
      log(`tick (${consecutiveFails} подряд): ${e}`)
      if (consecutiveFails >= 5) {
        log("5 тиков подряд упали — рестартуюсь (супервизор поднимет)")
        process.exit(1)
      }
    } finally {
      db?.close()
    }
    if (ONCE) {
      log("ONCE-режим: тик выполнен — выходим")
      process.exit(0)
    }
    await new Promise((r) => setTimeout(r, TICK_SEC * 1000))
  }
}

void main()
