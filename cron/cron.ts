// uwuwu-cron — планировщик крон-задач (SiYuan).
// Крон = обычная задача на доске «Задачи <proj>»: расписание — custom-атрибут
// custom-schedule в доке-теле, мастер живёт в статусе «Вейт». Демон поллит
// все задачные доски (собираются динамически из организ-дерева): строка
// Вейт + валидное расписание + наступило время → создаются ран-карточка
// (task create → Todo, запись запуска на доске) и OpenCode-сессия с промптом
// из тела мастера. Здоровый ран тих и закрывает свою карточку сам в финале
// (успех → Done; ран, оставшийся в Todo после тика, — сигнал сбоя); разовые
// (HH:MM [DD.MM]) после срабатывания снимают расписание — мастер не
// повторится (reconcile-гейт 24ч страхует окно снятия).
// Крон-доски «Кроны <proj>» и колонки Schedule/Status/Last Run сняты
// (унификация кронов с задачами, задача 20261106182051-o5ejlte).
// Запуск: systemd --user (uwuwu-cron.service). Тик: 1 минута.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, writeFileSync, renameSync, existsSync } from "node:fs"
import { homedir, hostname } from "node:os"
import { dirname, join, resolve } from "node:path"
import * as mcp from "../lib/platform"
import { checkNodePinning, failPinnedStart } from "../lib/node-pinning"
import { acquireLock, makeDrain, resolveDaemonStateDir, daemonLogPath, emitNotice } from "../lib/single-instance"
import { compactProbe, compactTick, sessionCompact, type CompactPolicy } from "../lib/compact"
import {
  due, fireGate, isOneShot, isValidTracked, cellPatches, formatLocal, parseSchedule, firePrompt, resolveBody, titleFromBody,
  stripNonContent, decideFire, agentDriftAction, resolveAgentIssue, parseAgentDefModel, runLandedAt, agentFirePrompt,
  type PlatformTool,
  type AgentModel, type CronRow, type CronState, type Sched, type SessionMsg, type TrackedCron,
} from "./cron-lib"

// state: песочница — UWUWU_CRON_STATE_DIR двигает state+lock, в песочнице лог
// тоже переезжает (прод-лог не засоряется смоук-тиками). Дефолт-изоляция:
// не-master чекаут без env получает свой state в branches/<ветка> (инцидент-
// класс 06.11); master — прежний прод-путь.
const STATE_RESOLVED = resolveDaemonStateDir({
  daemon: "uwuwu-cron",
  envName: "UWUWU_CRON_STATE_DIR",
  envRaw: process.env.UWUWU_CRON_STATE_DIR,
  defaultDir: join(homedir(), ".local/state/uwuwu-cron"),
  repoRoot: resolve(import.meta.dir, ".."),
})
const STATE_DIR = STATE_RESOLVED.stateDir
const STATE_FILE = join(STATE_DIR, "state.json")

const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
/** изолированный cwd крон-агентов: тело задачи редактируемо удалённо —
 *  исполняющая среда не должна видеть весь домашний каталог */
const WORKSPACE = join(homedir(), ".local/share/uwuwu-cron/workspace")
/** Путь лога: env-override (тесты тика пишут в tmp — os.homedir() в Bun
 *  не следует за runtime-подменой process.env.HOME, HOME-изоляция не работает);
 *  дефолт — sandbox-логика: в песочнице лог живёт в state-каталоге
 *  (прод-лог не засоряется смоук-тиками). */
const LOG_PATH = daemonLogPath({
  override: process.env.UWUWU_CRON_LOG, stateDir: STATE_DIR,
  sandboxed: STATE_RESOLVED.sandboxed, fileName: "uwuwu-cron.log", homeDir: homedir(),
})
const TICK_SEC = Number(process.env.UWUWU_CRON_TICK_SEC ?? 60)
const ONCE = process.env.UWUWU_CRON_ONCE === "1"
/** DRY_RUN: полный цикл решений по реальным доскам (чтение), спавны,
 *  ран-карточки и state-записи заменяются логом. Требует песочницу state —
 *  гард в main(). Без него песочный state опасен сам по себе: чистый state
 *  делает все периодические мастера due → реальные спавны на прод-досках. */
const DRY_RUN = process.env.UWUWU_CRON_DRY_RUN === "1"
/** пауза между срабатываниями одного мастера, пока жива его сессия */
const SESSION_COOLDOWN_MS = 10 * 60_000
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const

// автокомпакт вечных сессий (lib/compact): пороги из постановки владельца
// (задача 20261105153731-s6e27f6), env-оверрайды для проб; коулдаун — константа
const COMPACT_POLICY: CompactPolicy = {
  idleMs: (Number(process.env.UWUWU_CRON_COMPACT_IDLE_SEC ?? 14400) || 14400) * 1000,
  minContextPct: Number(process.env.UWUWU_CRON_COMPACT_MIN_PCT ?? 40) || 40,
  cooldownMs: 3_600_000,
}
const COMPACT_CHECK_SEC = Math.max(60, Number(process.env.UWUWU_CRON_COMPACT_CHECK_SEC ?? 600) || 600)

/** Бинарь CLI для ран-карточек: прод — ~/.cargo/bin (systemd PATH не несёт
 *  его); UWUWU_CLI_BIN переопределяет (тесты/нестандартная установка). */
const CLI_BIN = process.env.UWUWU_CLI_BIN
  ?? (existsSync(join(homedir(), ".cargo/bin/uwuwu-cli"))
    ? join(homedir(), ".cargo/bin/uwuwu-cli")
    : "uwuwu-cli")

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
}

/** task edit строки (колонки доски): единственный write-путь клеток — CLI.
 *  stdout или null (сбой — вызывающий решает: ретрай следующим тиком).
 *  Экспорт — юнит-тесты argv. */
export async function runCliEdit(project: string, rowId: string, args: string[]): Promise<string | null> {
  const proc = Bun.spawn([CLI_BIN, "task", "edit", rowId, "--project", project, ...args], {
    stdout: "pipe", stderr: "pipe",
  })
  const [out, err, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (code !== 0) {
    log(`task edit ${args.join(" ")} упал (code ${code}): ${err.trim().slice(0, 200)}`)
    return null
  }
  return out
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
  // инвариант «DRY_RUN ⇒ ни одной записи» держится на уровне API, не только
  // main()-гарда: прямой вызов tickBoard (тесты) в обход main не должен иметь
  // возможности тронуть прод-state (прод-кейс 06.11: DRY_RUN-тест без
  // UWUWU_CRON_STATE_DIR перезаписал прод-state.json тестовой записью)
  if (DRY_RUN) {
    log(`DRY_RUN: saveState (${Object.keys(state.crons).length} кронов) — пропущен`)
    return
  }
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

/** Задачные доски: «Задачи <proj>» по папкам Projects (динамически — новые
 *  проекты подхватываются без правки демона). Отсутствующие доски тихо
 *  скипаются (кроны живут там же, где обычные задачи). */
async function taskBoardTitles(): Promise<string[]> {
  try {
    const r = await mcp.tool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>(
      "list_organize_nodes", {})
    const nodes = r.nodes ?? []
    const pid = nodes.find(n => n.type === "folder" && n.data === "Projects")?.id
    const projects = nodes
      .filter(n => n.type === "folder" && n.parentId === pid)
      .map(n => n.data)
      .filter((d): d is string => typeof d === "string")
    return projects.map(p => `Задачи ${p}`)
  } catch (e) {
    if (e instanceof mcp.KernelUnavailableError) throw e // лежащее ядро не маскировать фоллбеком
    return []
  }
}

async function findBoard(title: string): Promise<BoardIds | null> {
  const r = await mcp.tool<{ matches?: { id: string; inTrash?: boolean }[] }>("find_doc_by_title", { title, caseInsensitive: true, limit: 5 })
  let docId = r.matches?.find(m => m.inTrash !== true)?.id
  if (docId === undefined) {
    // индекс тайтлов отстаёт от свежих/переименованных доков — фоллбек
    // через организ-дерево: «Задачи X» → папка X → дети по точному тайтлу
    const proj = title.startsWith("Задачи ") ? title.slice("Задачи ".length) : null
    if (proj !== null) docId = await boardViaOrganize(proj, title) ?? undefined
  }
  if (docId === undefined) return null
  const doc = await mcp.tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
  const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id
  if (dbId === undefined) return null
  return { docId, dbId }
}

/** Доска без поискового индекса: «Задачи <proj>» → папка <proj> → доки. */
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
  } catch (e) {
    if (e instanceof mcp.KernelUnavailableError) throw e // лежащее ядро не маскировать null-фоллбеком
    /* организ недоступен — остаётся null */
  }
  return null
}

async function findTaskBoards(): Promise<{ title: string; board: BoardIds }[]> {
  const out: { title: string; board: BoardIds }[] = []
  for (const title of await taskBoardTitles()) {
    try {
      const board = await findBoard(title)
      if (board !== null) out.push({ title, board })
    } catch (e) {
      if (e instanceof mcp.KernelUnavailableError) throw e // лежащее ядро не маскировать скипом доски
      /* доска недоступна — скип тика по ней */
    }
  }
  return out
}

/** Срез доски: строки задач (Title/Status/привязка тела). Расписание —
 *  не колонка: читается из атрибутов дока-тела отдельно, только для
 *  Вейт-строк (стоимость тика не растёт по всей доске). */
async function readRows(board: BoardIds): Promise<CronRow[]> {
  const r = await mcp.tool<{
    rows?: {
      rowBlockId?: string
      linkedDocId?: string | null
      title?: string | null
      cells?: Record<string, { value?: string; dateMs?: number | null }>
    }[]
  }>("read_database_cells", { docId: board.docId, databaseBlockId: board.dbId })
  return (r.rows ?? [])
    .filter((row) => typeof row.rowBlockId === "string")
    .map((row) => ({
      rowId: row.rowBlockId as string,
      bodyDoc: row.linkedDocId ?? "",
      title: (row.title ?? "").trim(),
      schedule: "",
      agent: "",
      plannedMs: row.cells?.Запланировано?.dateMs ?? null,
      scheduleCell: row.cells?.Расписание?.value ?? "",
      status: (row.cells?.Status?.value ?? "").toLowerCase(),
    }))
}

/** Свойства мастера из атрибутов дока-тела: расписание + базовый агент
 *  (один вызов get_doc_attrs на Вейт-строку, стоимость тика не растёт).
 *  schedule null — атрибута нет (обычная припаркованная в Вейт задача,
 *  не крон). REST getBlockAttrs, не SQL: колонка ial отстаёт от свежих записей. */
async function attrsOf(tool: PlatformTool, bodyDoc: string): Promise<{ schedule: string | null; agent: string }> {
  const attrs = await tool<{ [k: string]: string }>("get_doc_attrs", { docId: bodyDoc })
  return { schedule: attrs["custom-schedule"] ?? null, agent: attrs["custom-agent"] ?? "" }
}

// ——— opencode REST ———

async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = `Basic ${Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")}`
  const res = await fetch(`${base}/api${path}`, {
    method: init?.method ?? "GET",
    headers: { authorization: auth, "content-type": "application/json" },
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
    signal: AbortSignal.timeout(15_000),
  })
  const text = await res.text()
  if (!res.ok) throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
  if (text.trim() === "") return {}
  const j: unknown = mcp.parseJsonContract(text, `opencode ${path}: HTTP ${res.status}`)
  return (j as { data?: unknown }).data ?? j
}

async function fireSession(title: string, prompt: string): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: title.replace(/[\r\n]+/g, " ").slice(0, 60),
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

/** Статус вечной сессии против границы последнего приземления промпта:
 *  ход, начатый по idle-сессии, отдаёт стухшие outcome/idle до завершения
 *  (эмпирика hub sessionStatus) — граница решает. Сбой сети — running:
 *  стирт в живую сессию безопасен, POST в мёртвую вернётся pendingRun'ом.
 *  agent — вшитый при createAgentSession агент сессии (Agent.ID, живая
 *  проба 10.11): сверка с custom-agent мастера ловит смену карточки
 *  (эпизод 09.11); null — сервер не отдал поле/сбой — дрейф не засчитывать. */
async function sessionStatus(
  sessionID: string, boundaryMs: number,
): Promise<{ status: "idle" | "running" | "gone"; agent: string | null }> {
  try {
    const s = await restCall(`/session/${sessionID}`) as { outcome?: unknown; time?: { idle?: unknown }; agent?: unknown }
    const idle = s.time?.idle
    const status = s.outcome !== undefined && typeof idle === "number" && idle >= boundaryMs ? "idle" : "running"
    return { status, agent: typeof s.agent === "string" ? s.agent : null }
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) return { status: "gone", agent: null }
    return { status: "running", agent: null } // неизвестно — не блокируем
  }
}

/** Срез сообщений сессии (свежайшие; недоставленный хвост всегда в окне). */
async function sessionMessages(sessionID: string): Promise<SessionMsg[]> {
  const msgs = await restCall(`/session/${sessionID}/message?limit=100&order=desc`)
  return Array.isArray(msgs) ? msgs as SessionMsg[] : []
}

/** Признанные (admitted) входы между admission и потреблением агент-лупом:
 *  стирт в этом окне в /message ещё не виден — дедуп приземления слеп бы
 *  без inbox (паттерн hub sessionInbox). Сбой чтения — дедуп по /message. */
async function sessionInbox(sessionID: string): Promise<SessionMsg[]> {
  try {
    const entries = await restCall(`/session/${sessionID}/inbox`)
    if (!Array.isArray(entries)) return []
    return (entries as unknown[])
      .filter((e): e is Record<string, unknown> => e !== null && typeof e === "object")
      .filter((e) => e.type === "user")
      .map((e): SessionMsg | null => {
        const created = (e.time as { created?: unknown } | undefined)?.created
        const text = (e.payload as Record<string, unknown> | undefined)?.text
        if (typeof text !== "string" || text.trim() === "" || typeof created !== "number" || !Number.isFinite(created)) return null
        return { type: "user", time: { created }, text }
      })
      .filter((m): m is SessionMsg => m !== null)
  } catch {
    return []
  }
}

/** POST промпта в сессию; time.created ответа — точная граница приземления
 *  (стирт по idle-сессии отдаёт стухший outcome/idle — граница берётся из
 *  ответа, паттерн hub steerPost). null — не приземлился. */
async function promptPost(sessionID: string, prompt: string, steer: boolean): Promise<number | null> {
  try {
    const res = await restCall(`/session/${sessionID}/prompt`, {
      method: "POST",
      body: { text: prompt, ...(steer ? { delivery: "steer" } : {}) },
    }) as { time?: { created?: unknown } }
    const created = res.time?.created
    return typeof created === "number" && Number.isFinite(created) && created > 0 ? created : Date.now()
  } catch {
    return null
  }
}

/** Новая вечная сессия мастера: деф агента (нативный agent:name) + модель
 *  из фронта дефа (opencode не применяет её к primary-сессиям сам —
 *  передаём явно; null — модель демона). */
async function createAgentSession(
  agent: string, masterTitle: string, model: AgentModel | null,
): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: masterTitle.replace(/[\r\n]+/g, " ").slice(0, 60),
      agent,
      model: model ?? MODEL,
      location: { directory: WORKSPACE },
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  return typeof id === "string" && id.startsWith("ses_") ? id : null
}

/** Деф агента: существование (файр отказом при отсутствии) + модель фронта. */
function agentDefOf(agent: string): { exists: boolean; model: AgentModel | null } {
  const path = join(homedir(), ".config/opencode/agents", `${agent}.md`)
  if (!existsSync(path)) return { exists: false, model: null }
  try {
    return { exists: true, model: parseAgentDefModel(readFileSync(path, "utf-8")) }
  } catch {
    return { exists: true, model: null }
  }
}

/** Пересоздание вечной сессии мастера под текущий custom-agent: новая
 *  сессия + DELETE старой (note ≠ null — сессия была жива: сироту в
 *  opencode не оставляем; note null — gone-путь, старая уже мертва).
 *  null — не поднялась (state вызывающий не трогает — ретрай следующим
 *  тиком). */
async function recreateAgentSession(
  oldSessionID: string, agent: string, masterTitle: string, model: AgentModel | null, note: string | null,
): Promise<string | null> {
  const created = await createAgentSession(agent, masterTitle, model)
  if (created === null) {
    log(`мастер «${masterTitle}»: вечная сессия не поднялась — ретрай следующим тиком`)
    return null
  }
  if (oldSessionID === "") return created // первосоздание — старой нет
  if (note === null) {
    log(`мастер «${masterTitle}»: сессия ${oldSessionID} пропала — пересоздана ${created} (контекст потерян)`)
  } else {
    await restCall(`/session/${oldSessionID}`, { method: "DELETE" }).catch(() => {})
    log(`мастер «${masterTitle}»: ${note} — сессия ${oldSessionID} пересоздана как ${created} (контекст сброшен)`)
  }
  return created
}

// ——— тик ———

async function tick(state: CronState): Promise<void> {
  const boards = await findTaskBoards()
  const now = new Date()
  for (const { title: boardTitle, board } of boards) {
    await tickBoard(state, board, boardTitle, now)
  }
}

/** Проект из тайтла доски («Задачи <proj>»): явный --project в дологе
 *  рана и комментов — агенту не нужно угадывать. */
function projectOfBoard(boardTitle: string): string {
  return boardTitle.startsWith("Задачи ") ? boardTitle.slice("Задачи ".length) : "uwuwu"
}

/** Тайтл ран-карточки: метка времени делает его уникальным на доске
 *  (дубли тайтлов строк CLI ловит как ошибку). */
function runCardTitle(master: CronRow, now: Date): string {
  const sec = String(now.getSeconds()).padStart(2, "0")
  return `⏰ ${master.title || master.rowId.slice(0, 8)} · ${formatLocal(now)}:${sec}`
}

/** Тело ран-карточки: шапка запуска + копия инструкции мастера на момент
 *  файра (владелец видит, ЧТО выполнялось; промпт сессии несёт то же тело). */
export function runCardBody(masterTitle: string, schedule: string, instruction: string, now: Date): string {
  return [
    `Ран крон-задачи «${masterTitle.replace(/[\r\n]+/g, " ")}» (расписание: \`${schedule}\`), запуск ${formatLocal(now)}.`,
    "Успешный прогон закрывает агент рана (Done); ниже — копия инструкции мастера на момент файра.",
    "",
    "## Инструкция мастера",
    instruction,
  ].join("\n")
}

/** Ран-карточка запуска: task create --status Todo → edit (тип+planned).
 *  Начальный статус Todo прямо в create — Draft-фазы рана нет, draftLoop
 *  триажа (строгий матч Status === "Draft") раны не видит (жалоба 12.11:
 *  Draft-раны доставлялись владельцу в ТГ). CLI зовётся процессом — боевой
 *  create с дедупом тайтла и verify вставки; дублировать write-путь в TS
 *  не делаем. Дата файра — колонка «Запланировано» (минутная точность,
 *  формат сравнения reconcile). rowId или null (создание не удалось —
 *  ретрай следующим тиком). */
export async function createRunCard(project: string, runTitle: string, bodyMd: string, firedAt: Date): Promise<string | null> {
  const create = Bun.spawn([CLI_BIN, "task", "create", "--project", project, "--title", runTitle, "--status", "Todo", "--stdin"], {
    stdin: "pipe", stdout: "pipe", stderr: "pipe",
  })
  create.stdin.write(bodyMd)
  create.stdin.end()
  const [out, err, code] = await Promise.all([
    new Response(create.stdout).text(),
    new Response(create.stderr).text(),
    create.exited,
  ])
  if (code !== 0) {
    log(`task create упал (code ${code}): ${err.trim().slice(0, 200)}`)
    return null
  }
  // контракта ответов (PR #226): create печатает шапку карточки — rowId в строке «Строка:   <id>»
  const rowId = /Строка:\s+(\S+)/.exec(out)?.[1]
  if (rowId === undefined) {
    log(`task create без строки в выводе: ${out.trim().slice(0, 200)}`)
    return null
  }
  // ран — запись запуска, тип не несёт: клетка «Тип» от create-дефолта
  // (feature) снимается тем же edit — без неё канбан-фильтры по типу
  // не шумят ранами
  const edit = Bun.spawn([CLI_BIN, "task", "edit", rowId, "--project", project, "--type", "", "--planned", formatLocal(firedAt)], {
    stdout: "pipe", stderr: "pipe",
  })
  const [, editErr, editCode] = await Promise.all([
    new Response(edit.stdout).text(),
    new Response(edit.stderr).text(),
    edit.exited,
  ])
  if (editCode !== 0) {
    // не фатально: ран уже Todo (создан таким), draftLoop его не видит —
    // доставки владельцу нет; потери: клетка «Тип» (feature) и дата файра
    // в «Запланировано». Пустая клетка даты видна на доске и правится
    // руками — отдельного ретрая не строим.
    log(`task edit type/planned не прошёл (code ${editCode}): ${editErr.trim().slice(0, 200)} — ран Todo, клетка тип/planned не записана`)
  }
  return rowId
}

/** Экспорт для юнит-тестов тика (демон стартует только как entry — import.meta.main). */
/** Исход сбойного тика для главного цикла. Wave (KernelWaveError: ядро в волне
 *  деградации, окно fail-fast) — штатная пауза контура: consecutiveFails не
 *  трогается (ни инкремент, ни сброс — реальные фейлы по соседству копятся
 *  и приведут к рестарту честно), сессия не пересобирается (ядро живо,
 *  отвечает ошибками), рестарта нет — окно разрядится само, а рестарт
 *  сбрасывал бы окно и реанимировал ретраи (TICK_SEC=60 при окне 90s давал
 *  бы exit(1) на 5-м тике любой длинной волны). Прочие сбои копят счётчик:
 *  5 подряд → рестарт (отравленное соединение/пул — свежий процесс чище). */
export function tickFailOutcome(e: unknown, consecutiveFails: number): { fails: number; reconnect: boolean; restart: boolean } {
  if (e instanceof mcp.KernelWaveError) return { fails: consecutiveFails, reconnect: false, restart: false }
  const fails = consecutiveFails + 1
  return { fails, reconnect: true, restart: fails >= 5 }
}

/** Reconcile клеток мастера: расхождение → одна CLI-запись обоих клеток.
 *  DRY_RUN — только лог. */
async function reconcileMasterCells(project: string, row: CronRow, sched: Sched, prevFire: number, now: Date): Promise<void> {
  const patch = cellPatches(row, sched, prevFire, now)
  const args: string[] = []
  if (patch.schedule !== undefined) args.push("--schedule", patch.schedule)
  if (patch.planned !== undefined) args.push("--planned", patch.planned)
  if (args.length === 0) return
  log(`мастер «${row.title || row.rowId.slice(0, 8)}»: клетки доски не сходятся (${args.join(" ")}) — синхронизирую`)
  if (DRY_RUN) {
    log(`DRY_RUN: запись клеток пропущена`)
    return
  }
  await runCliEdit(project, row.rowId, args)
}

// ——— вечная сессия (custom-agent) ———

/** Отказ файра вечного мастера с дедупом коммента (коммент — только при
 *  смене причины, сброс при успешном файре); слот НЕ сжигается — починка
 *  причины (деф заведён, расписание поправлено) подхватывается следующим
 *  слотом. Решение фиксируется in-memory и в DRY_RUN (паритет кривого
 *  расписания: решения не отменяются), записи (коммент/state.json) — нет. */
async function refuseOnce(state: CronState, row: CronRow, reason: string): Promise<void> {
  const prev = state.crons[row.rowId]
  if (prev?.lastRefusal === reason) return
  if (!DRY_RUN) {
    await mcp.tool("create_comment", {
      docId: row.bodyDoc,
      content: `крон-мастер не запускается: ${reason}`,
    })
  }
  state.crons[row.rowId] = { ...(prev ?? { sessionID: "", lastFiredAt: 0, firedAtIso: "" }), lastRefusal: reason }
  if (!DRY_RUN) saveState(state)
}

/** Ретрай файра с неподтверждённым POST (дедуп приземления — паттерн hub
 *  steerLandedAt: «сервер приземлил промпт, ответ потерян» — повторный POST
 *  продублировал бы инструкцию в вечный контекст). Дедуп по сигнатуре рана
 *  (runLandedAt): приземлившаяся копия конверта с любым текстом (ручной
 *  стирт-ретрай оператора) признаётся. Приземлился — файр завершён
 *  (lastFiredAt по времени приземления); мёртвая сессия — pendingRun
 *  дропается, следующий файр начнётся заново; дрейф агента — recreate под
 *  нового агента с доставкой ИСХОДНОГО конверта (ран не дублируется). */
async function resolvePendingRun(state: CronState, tracked: TrackedCron, row: CronRow, masterTitle: string): Promise<void> {
  const pending = tracked.pendingRun
  if (pending === undefined) return
  if (tracked.sessionID === "") {
    log(`мастер «${masterTitle}»: pendingRun без сессии — дроп, файр начнётся заново (ран ${pending.rowId} ждёт ретрая)`)
    state.crons[row.rowId] = { sessionID: "", lastFiredAt: tracked.lastFiredAt, firedAtIso: tracked.firedAtIso }
    saveState(state)
    return
  }
  const probe = await sessionStatus(tracked.sessionID, pending.attemptAt)
  if (probe.status === "gone") {
    log(`мастер «${masterTitle}»: сессия ${tracked.sessionID} пропала — pendingRun дроп, контекст файра утерян`)
    state.crons[row.rowId] = { sessionID: "", lastFiredAt: tracked.lastFiredAt, firedAtIso: tracked.firedAtIso }
    saveState(state)
    return
  }
  const drift = agentDriftAction(probe.status, probe.agent, row.agent)
  let action = decideFire(probe.status)
  if (drift === "recreate") {
    // агент карточки сменён при файре в полёте (эпизод 09.11): POST в старую
    // сессию умирает тихо «Agent not found». Пересоздание под нового агента,
    // исходный конверт доставляется в новую сессию — ран не теряется.
    // Дедуп против СТАРОЙ сессии сознательно пропущен: приземлившийся там
    // конверт не отличить от умершего хода («Agent not found») — ретрай в
    // свежую гарантирует исполнение, ценой редкого двойного исполнения рана
    // (POST-ответ потерян + смена агента + старый ран успел завершиться;
    // дубль видимый, не тихий)
    const def = agentDefOf(row.agent)
    const issue = resolveAgentIssue(row.agent, def.exists)
    if (issue !== null) {
      log(`мастер «${masterTitle}»: ${issue}`)
      await refuseOnce(state, row, issue)
      return
    }
    const created = await recreateAgentSession(
      tracked.sessionID, row.agent, masterTitle, def.model,
      `агент сменён (${probe.agent} → ${row.agent})`,
    )
    if (created === null) return // state не тронут — ретрай пересоздания следующим тиком
    tracked.sessionID = created // tracked — живая ссылка state.crons[row.rowId]
    saveState(state)
    action = "prompt" // свежая сессия пуста — обычный POST, не стирт
  } else if (drift === "defer") {
    log(`мастер «${masterTitle}»: агент сменён (${probe.agent} → ${row.agent}) на живом ходе — ход не рвём, пересоздание на следующем файре`)
  }
  let msgs = await sessionMessages(tracked.sessionID).catch((e: unknown) => {
    log(`мастер «${masterTitle}»: сообщения не достали (${String(e)}) — дедуп по inbox`)
    return [] as SessionMsg[]
  })
  msgs = [...msgs, ...(await sessionInbox(tracked.sessionID))]
  // граница дедупа — прошлый файр (lastFiredAt), не attemptAt: ран-карточка
  // создана позже него, конверт этого рана не мог приземлиться раньше, а окно
  // шире — ручная копия между попытками доставки (лаг REST, стирт оператора
  // до штампа pendingRun) дедупом не теряется
  const landed = runLandedAt(msgs, tracked.lastFiredAt, pending.rowId)
  if (landed !== null) {
    log(`мастер «${masterTitle}»: промпт файра приземлился ранее (${landed}) — ретрай не нужен (ран ${pending.rowId})`)
    state.crons[row.rowId] = { sessionID: tracked.sessionID, lastFiredAt: landed, firedAtIso: new Date(landed).toISOString() }
    saveState(state)
    return
  }
  const landedMs = await promptPost(tracked.sessionID, pending.prompt, action === "steer")
  if (landedMs === null) {
    // новая метка попытки: граница sessionStatus для decideFire следующего
    // ретрая (outcome/idle позже метки = ход завершился, можно обычный POST).
    // Дедуп приземления от неё НЕ идёт — его граница lastFiredAt (см. выше)
    pending.attemptAt = Date.now()
    saveState(state)
    log(`мастер «${masterTitle}»: ретрай файра не приземлился — ретрай следующим тиком (ран ${pending.rowId})`)
    return
  }
  log(`мастер «${masterTitle}»: ретрай файра приземлился (${landedMs}) — ран ${pending.rowId}`)
  state.crons[row.rowId] = { sessionID: tracked.sessionID, lastFiredAt: landedMs, firedAtIso: new Date(landedMs).toISOString() }
  saveState(state)
}

const modelLabel = (model: AgentModel | null): string =>
  model === null ? "демона" : `${model.providerID}/${model.id}${model.variant !== undefined ? `#${model.variant}` : ""}`

// ——— автокомпакт вечной сессии мастера ———

/** Троттлинг компакт-чека (in-memory: пропуск из-за рестарта догонит следующий
 *  тик; сама admission-метка персистится в tracked.lastCompactAt). */
const compactLastCheck = new Map<string, number>()

/** Чек автокомпакта вечной сессии: занятость — через границу lastFiredAt
 *  (стухший idle хода, паттерн sessionStatus), модель/лимит/токены —
 *  compactProbe, оркестрация «персист метки до POST» — compactTick.
 *  DRY_RUN/ONCE — ноль реальных действий (инвариант демона: ручной/песочный
 *  тик не должен POST'ить): решения логом, GET/POST/state не звать. */
async function maybeCompactEternal(state: CronState, row: CronRow, tracked: TrackedCron): Promise<void> {
  const masterTitle = row.title || row.rowId.slice(0, 8)
  if (DRY_RUN || ONCE) {
    log(`мастер «${masterTitle}»: ${DRY_RUN ? "DRY_RUN" : "ONCE"} — компакт-чек пропущен (порог idle ${Math.round(COMPACT_POLICY.idleMs / 60_000)}м, контекст ≥${COMPACT_POLICY.minContextPct}%)`)
    return
  }
  await compactTick({
    sessionID: () => tracked.sessionID,
    busy: () => false, // занятость решает граница lastFiredAt в probe
    probe: async (sessionID) => {
      const p = await compactProbe(restCall, sessionID)
      const idleOk = p.idleAt !== null && p.idleAt >= tracked.lastFiredAt
      return { idleMs: idleOk ? Date.now() - (p.idleAt as number) : null, usagePct: p.usagePct }
    },
    lastCompactAt: () => tracked.lastCompactAt ?? 0,
    persist: (ms) => {
      state.crons[row.rowId] = { ...tracked, lastCompactAt: ms }
      saveState(state)
    },
    fire: (sessionID) => sessionCompact(restCall, sessionID),
    policy: COMPACT_POLICY,
    log: (m) => log(`мастер «${masterTitle}»: ${m}`),
  })
}

/** Файр вечного мастера: ран-карточка — как у изолированного контура, промпт
 *  уходит в ОДНУ сессию мастера (custom-agent): idle → ход, running → стирт,
 *  gone → пересоздание (контекст потерян — честный лог). Дрейф агента
 *  (смена custom-agent карточки при живой сессии, эпизод 09.11): idle →
 *  recreate под нового агента, running → ход не рвём (пересоздание на
 *  следующем файре). Неподтверждённый POST — pendingRun (ретрай следующим
 *  тиком, ран не дублируется). */
async function fireAgentMaster(state: CronState, input: {
  project: string
  row: CronRow
  sched: Sched
  body: string
  masterTitle: string
  now: Date
}): Promise<void> {
  const { project, row, sched, body, masterTitle, now } = input
  // разовое + вечная сессия не сочетаются: снятое после файра расписание
  // уводит строку из крон-пула — ретрай pendingRun потерял бы тики
  if (isOneShot(sched)) {
    const reason = "разовое расписание (HH:MM) не сочетается с вечной сессией (custom-agent) — жди интервал 5m|2h|1d или cron из 5 полей"
    log(`мастер «${masterTitle}»: ${reason}`)
    await refuseOnce(state, row, reason)
    return
  }
  const def = agentDefOf(row.agent)
  const issue = resolveAgentIssue(row.agent, def.exists)
  if (issue !== null) {
    log(`мастер «${masterTitle}»: ${issue}`)
    await refuseOnce(state, row, issue)
    return
  }
  if (DRY_RUN) {
    const promptLen = agentFirePrompt({ title: row.title, schedule: row.schedule }, body, "0000000000000000-run", project).length
    log(`DRY_RUN: вечный мастер «${masterTitle}» (agent «${row.agent}», модель ${modelLabel(def.model)}) — ран-карточка и файр не созданы (промпт ${promptLen} байт)`)
    return
  }
  const runTitle = runCardTitle(row, now)
  const runRowId = await createRunCard(project, runTitle, runCardBody(row.title, row.schedule, body, now), now)
  if (runRowId === null) {
    log(`мастер «${masterTitle}»: ран-карточка не создана — ретрай следующим тиком`)
    return
  }
  const prompt = agentFirePrompt({ title: row.title, schedule: row.schedule }, body, runRowId, project)
  const tracked = state.crons[row.rowId] ?? { sessionID: "", lastFiredAt: 0, firedAtIso: "" }
  const probe = tracked.sessionID === ""
    ? { status: "gone" as const, agent: null }
    : await sessionStatus(tracked.sessionID, tracked.lastFiredAt)
  const { status, agent: liveAgent } = probe
  const drift = status === "gone" ? null : agentDriftAction(status, liveAgent, row.agent)
  if (drift === "defer") {
    log(`мастер «${masterTitle}»: агент сменён (${liveAgent} → ${row.agent}) на живом ходе — ход не рвём, пересоздание на следующем файре`)
  }
  const action = drift === "recreate" ? "recreate" : decideFire(status)
  let sessionID = tracked.sessionID
  if (action === "recreate") {
    const created = await recreateAgentSession(
      tracked.sessionID, row.agent, masterTitle, def.model,
      drift === "recreate" ? `агент сменён (${liveAgent} → ${row.agent})` : null,
    )
    if (created === null) return
    sessionID = created
  }
  const landedMs = await promptPost(sessionID, prompt, action === "steer")
  if (landedMs === null) {
    // ран-карточка уже создана: pendingRun хранит её rowId — ретрай не плодит дубли
    state.crons[row.rowId] = {
      sessionID, lastFiredAt: tracked.lastFiredAt, firedAtIso: tracked.firedAtIso,
      pendingRun: { rowId: runRowId, prompt, attemptAt: Date.now() },
    }
    saveState(state)
    log(`мастер «${masterTitle}»: файр не приземлился — pendingRun, ретрай следующим тиком (ран ${runRowId})`)
    return
  }
  state.crons[row.rowId] = { sessionID, lastFiredAt: landedMs, firedAtIso: new Date(landedMs).toISOString() }
  saveState(state)
  log(`мастер «${masterTitle}»: ран ${runRowId} → вечная сессия ${sessionID} (${action}, приземлился ${landedMs})`)
}

export async function tickBoard(state: CronState, board: BoardIds, boardTitle: string, now: Date): Promise<void> {
  const rows = await readRows(board)
  const project = projectOfBoard(boardTitle)

  for (const row of rows) {
    if (row.status !== "вейт") continue
    // Вейт без тела или без custom-schedule — обычная припаркованная задача
    // (Вейт общий для некронов), не крон-мастер: тихий скип
    if (row.bodyDoc === "") continue
    let scheduleRaw: string
    try {
      const attrs = await attrsOf(mcp.tool, row.bodyDoc)
      if (attrs.schedule === null) continue
      scheduleRaw = attrs.schedule
      row.agent = attrs.agent
    } catch (e) {
      if (e instanceof mcp.KernelUnavailableError) throw e
      log(`мастер «${row.title || row.rowId.slice(0, 8)}»: атрибуты тела не прочитаны (${String(e)}) — пропуск, ретрай следующим тиком`)
      continue
    }
    row.schedule = scheduleRaw

    const sched0 = parseSchedule(scheduleRaw)
    // reconcile: разовый уже стрелял (демон упал между карточкой и снятием
    // расписания) → не повторяем в течение окна; штатно расписание снято
    // сразу после карточки и мастер уходит из выборки до этого гейта
    const prevFire = state.crons[row.rowId]?.lastFiredAt ?? 0
    if (sched0 !== null && isOneShot(sched0) && prevFire > 0
        && now.getTime() - prevFire < 86_400_000) {
      log(`мастер «${row.title || row.rowId.slice(0, 8)}»: разовый уже стрелял — скип (reconcile)`)
      continue
    }
    const sched = sched0
    if (sched === null) {
      // диагностика: молчащий навсегда мастер — худший режим отказа;
      // коммент с дедупом (не спамим каждым тиком). Слот НЕ сжигается:
      // фиксированное расписание не должно упираться в отказное prevFire
      // (reconcile разового ложно «стрелял» бы)
      const reason = `кривое расписание «${scheduleRaw}» — жди 5m|2h|1d, HH:MM [DD.MM] или cron из 5 полей`
      log(`мастер «${row.title || row.rowId.slice(0, 8)}»: ${reason}`)
      const prev = state.crons[row.rowId]
      if (prev?.lastRefusal !== reason) {
        if (DRY_RUN) {
          log(`DRY_RUN: коммент отказа пропущен`)
        } else {
          await mcp.tool("create_comment", {
            docId: row.bodyDoc,
            content: `крон-мастер не запускается: ${reason}. Почини расписание (task edit ${row.rowId} --schedule "…") — следующий файр подхватит.`,
          })
        }
        state.crons[row.rowId] = {
          ...(prev ?? { sessionID: "", lastFiredAt: 0, firedAtIso: "" }),
          lastRefusal: reason,
        }
        if (!DRY_RUN) saveState(state)
      }
      continue
    }
    // клетки мастера сверяются каждый тик (не только при файре): смена
    // расписания и дрейф клетки сходятся за минуту, доска не врёт
    await reconcileMasterCells(project, row, sched, state.crons[row.rowId]?.lastFiredAt ?? 0, now)

    const tracked = state.crons[row.rowId]
    const lastFired = tracked?.lastFiredAt ?? 0

    // автокомпакт вечной сессии — ДО due-гейта: основной юзкейс — idle-окна
    // между файрами (после гейта чек доживал бы только до моментов файра,
    // когда сессия занята, и не выстреливал бы никогда). Файр в полёте
    // (pendingRun) приоритетнее — вечный контур его доигрывает отдельно.
    if (row.agent !== "" && tracked !== undefined && tracked.sessionID !== "" && tracked.pendingRun === undefined) {
      const lastCheck = compactLastCheck.get(row.rowId) ?? 0
      if (now.getTime() - lastCheck >= COMPACT_CHECK_SEC * 1000) {
        compactLastCheck.set(row.rowId, now.getTime())
        await maybeCompactEternal(state, row, tracked)
      }
    }

    // предыдущая изолированная сессия ещё жива → не спамим (кроме давно
    // повисших). Вечные сессии не гейтятся: занятость решает стирт, а
    // pendingRun-ретрай обязан идти каждым тиком независимо от due
    if (row.agent === "" && tracked !== undefined && tracked.sessionID !== "") {
      const alive = await sessionAlive(tracked.sessionID)
      if (alive === true && now.getTime() - lastFired < SESSION_COOLDOWN_MS) continue
      if (alive === null) continue
    }

    // вечный мастер с файром в полёте: дедуп приземления/ретрай до due-гейта —
    // неподтверждённый POST не должен ждать слота расписания. DRY_RUN — гард:
    // resolvePendingRun делает реальные opencode-REST (sessionStatus/promptPost)
    // в боевой сервис — инвариант «DRY_RUN ⇒ ноль реальных действий» важнее
    // доигрывания ретрая (pendingRun остаётся в state песочницы)
    if (row.agent !== "" && tracked?.pendingRun !== undefined) {
      if (DRY_RUN) {
        log(`DRY_RUN: мастер «${row.title || row.rowId.slice(0, 8)}» — pendingRun-ретрай пропущен (файр ${tracked.pendingRun.rowId} остаётся в полёте)`)
        continue
      }
      await resolvePendingRun(state, tracked, row, row.title || row.rowId.slice(0, 8))
      continue
    }

    if (!due(sched, lastFired, now)) continue
    // стоп-сигнал: не спавним новую работу — мастер доедет при следующем старте
    if (drain.isStopping()) break

    // срез доски прочитан в начале тика — перед спавном строка перепроверяется:
    // мастер мог быть закрыт/снят с Вейт, пока тик обрабатывал очередь
    const gate = fireGate(await readRows(board), row.rowId)
    if (gate !== "fire") {
      log(`мастер «${row.title || row.rowId.slice(0, 8)}»: ${gate === "removed" ? "строка снята" : "статус не Вейт"} — пропуск`)
      continue
    }
    const resolved = await resolveBody(mcp.tool, board.docId, row)
    const masterTitle = row.title !== "" ? row.title
      : titleFromBody(resolved.kind === "ok" ? resolved.markdown : "") || `мастер ${row.rowId.slice(0, 8)}`
    if (resolved.kind === "infra") {
      // сбой ядра ≠ вердикт «тела нет»: state не трогаем — ретрай следующим
      // тиком (пропущенная минута ждёт следующей матчера-минуты, семантика
      // классического cron)
      log(`мастер «${masterTitle}»: тело не разрешено (сбой ядра?) — ${resolved.error} — ретрай следующим тиком`)
      continue
    }
    if (resolved.kind === "refuse") {
      // отказ вместо спавна: коммент причины в тело мастера с дедупом.
      // Слот сжигается (lastFiredAt=now): due уже прошёл — без сожжения
      // каждый тик той же матчера-минуты повторял бы отказ; ретрай — на
      // следующем слоте расписания (семантика старого контура)
      log(`мастер «${masterTitle}»: отказ — ${resolved.reason}`)
      const deduped = state.crons[row.rowId]?.lastRefusal === resolved.reason
      // решения не отменяются и в DRY_RUN: lastRefusal в in-memory state,
      // записи (коммент/state.json) — заменяются логом
      state.crons[row.rowId] = {
        sessionID: "", lastFiredAt: now.getTime(), firedAtIso: now.toISOString(),
        lastRefusal: resolved.reason,
      }
      if (DRY_RUN) {
        log(`DRY_RUN: коммент отказа пропущен`)
      } else {
        if (!deduped) {
          await mcp.tool("create_comment", {
            docId: row.bodyDoc,
            content: `крон-мастер «${masterTitle}» не запущен: ${resolved.reason}. Почини постановку (тело/привязку строки) — следующий файр подхватит.`,
          })
        }
        saveState(state)
      }
      continue
    }
    // DRY_RUN: до спавна — мастер due, гейт пройден, тело валидно; ран и
    // сессию не создаём
    if (row.agent !== "") {
      await fireAgentMaster(state, { project, row, sched, body: stripNonContent(resolved.markdown), masterTitle, now })
      continue
    }
    if (DRY_RUN) {
      const promptLen = firePrompt({ title: row.title, schedule: row.schedule }, resolved.markdown, "0000000000000000-run", project).length
      log(`DRY_RUN: мастер «${masterTitle}» — ран-карточка и сессия не созданы (промпт ${promptLen} байт)`)
      continue
    }
    const runTitle = runCardTitle(row, now)
    const instruction = stripNonContent(resolved.markdown)
    const runRowId = await createRunCard(project, runTitle, runCardBody(row.title, row.schedule, instruction, now), now)
    if (runRowId === null) {
      log(`мастер «${masterTitle}»: ран-карточка не создана — ретрай следующим тиком`)
      continue
    }
    // факт файра фиксируется созданной карточкой: дубль-ран на упавшей
    // сессии хуже пропущенного рана (карточка = запись запуска)
    state.crons[row.rowId] = { sessionID: "", lastFiredAt: now.getTime(), firedAtIso: now.toISOString() }
    saveState(state)
    if (isOneShot(sched)) {
      // разовый: расписание снимается сразу после карточки — мастер покидает
      // крон-пул (старый контур писал Status=fired в ячейку). Снятие ДО
      // спавна: крах между шагами не даст дубль-ран следующим тиком. Повтор
      // — заново задать расписание (task edit --schedule). Снятие — через
      // CLI: он единственный write-путь колонок, чистит и attr, и ОБЕ клетки
      // («Расписание», «Запланировано») одной командой.
      const cleared = await runCliEdit(project, row.rowId, ["--schedule", "", "--planned", ""])
      if (cleared !== null) {
        log(`мастер «${masterTitle}»: разовый — расписание снято (повтор: task edit ${row.rowId} --schedule "…")`)
      } else {
        // reconcile-гейт (окно 24ч по prevFire) прикроет рефайр до следующего тика
        log(`мастер «${masterTitle}»: не смог снять расписание разового — держит reconcile-гейт`)
      }
    }
    const prompt = firePrompt({ title: row.title, schedule: row.schedule }, resolved.markdown, runRowId, project)
    const sessionID = await fireSession(runTitle, prompt)
    if (sessionID === null) {
      log(`мастер «${masterTitle}»: ран ${runRowId} создан, сессия не поднялась`)
      continue
    }
    state.crons[row.rowId].sessionID = sessionID
    saveState(state)
    log(`мастер «${masterTitle}»: ран ${runRowId} → сессия ${sessionID}`)
  }
}

async function main(): Promise<void> {
  // env шлюза обязателен на старте (раньше это делал process.exit на импорте
  // mcp.ts — теперь проверка явная, модуль шарится с ленивой конфигурацией)
  if (!mcp.mcpConfigured()) {
    console.error("mcp: нужен UWUWU_SIYUAN_TOKEN (или UWUWU_SIYUAN_WS)")
    process.exit(1)
  }
  // гард dry-run ДО лока: невалидный режим не должен даже притворяться
  // демоном (фиктивные записи не должны попадать в прод-state). Эффективная
  // песочница — с авто-изоляцией ветки: там прод-state и так недостижим.
  if (DRY_RUN && !STATE_RESOLVED.sandboxed) {
    console.error("DRY_RUN требует UWUWU_CRON_STATE_DIR (песочница state) — фиктивные записи не должны попадать в прод-state")
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: drain.stop })) {
    console.error("другой живой инстанс uwuwu-cron держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  mkdirSync(WORKSPACE, { recursive: true })
  await mcp.connect()
  // пиннинг за нодами (ADR-076): крон — singleton-класс, второй экземпляр
  // файрил бы кроны дважды; строка «cron: нода» в доке — контракт
  const pinning = await checkNodePinning(mcp.tool, {
    singletonDaemon: "cron",
    envNodeName: process.env.UWUWU_NODE_NAME,
    hostname: hostname(),
  })
  failPinnedStart(pinning)
  log(`пиннинг: нода ${pinning.nodeName} подтверждена`)
  log(`uwuwu-cron: старт (задачные доски «Задачи <proj>», гейт Вейт, tick=${TICK_SEC}s, model=${MODEL.id}${DRY_RUN ? ", DRY_RUN" : ""}${STATE_RESOLVED.sandboxed ? `, state=${STATE_DIR}` : ""})`)
  // громкое замечание резолюции state — в лог и stderr (journald)
  emitNotice(STATE_RESOLVED.notice, log)
  const state = loadState()
  log(`кронов в state: ${Object.keys(state.crons).length}`)
  let consecutiveFails = 0
  while (true) {
    try {
      await tick(state)
      consecutiveFails = 0
    } catch (e) {
      const outcome = tickFailOutcome(e, consecutiveFails)
      const isWave = e instanceof mcp.KernelWaveError
      consecutiveFails = outcome.fails
      // сообщение и стек отдельными строками: bun у подклассов Error рендерит
      // stack без сообщения (прод-кейс 05.11: «tick: Error» без причины)
      log(`tick (${consecutiveFails} подряд${isWave ? ", wave" : ""}): ${e instanceof Error ? `${e.name}: ${e.message}\n${e.stack ?? ""}` : String(e)}`)
      if (outcome.restart) {
        // отравленное соединение/пул: свежий процесс чище вечных ретраев
        log("5 тиков подряд упали — рестартуюсь (systemd поднимет)")
        process.exit(1)
      }
      if (outcome.reconnect) await mcp.connect().catch(() => {}) // попробовать пересобрать сессию сразу
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

if (import.meta.main) void main()
