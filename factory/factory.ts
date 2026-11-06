// uwuwu-factory — будильник фабрики разработки + маршрутизатор ревью (SiYuan).
// Вся логика пайпа — у агентов (глобальный AGENTS.md, раздел «Работа над
// задачей»). Демон делает четыре вещи:
//   1. Design на доске (Draft = черновик на валидации триажа, пустой =
//      легаси-черновик юзера — оба не трогаем) → сессия девелопера в
//      чекауте репо + spawn-промпт утряски (ворктри/ветку создаёт сам
//      агент); In Progress без записи state — fast-track владельца →
//      implement-промпт (сразу реализация, без Design/план-ревью)
//   2. «Ревью плана»/«Ревью результата» → спавн/будильник ревьювера
//      (праймари code-quality-reviewer, 1 сессия на задачу, cwd = ворктри
//      задачи); возврат статуса из ревью → wakeup девелопера; комменты
//      юзера в ревью-статусах — ревьюверу
//   3. новые комменты/правки тела юзера → wakeup сессии (мёртвая → карантин,
//      коммент юзера → рестарт)
//   4. Done (кто бы ни поставил — мерж-крон агента или юзер) → чистка
//      state + термин живой ревью-сессии
//   5. In Progress: сессия жива, юзер и агент молчат дольше порога
//      (UWUWU_FACTORY_STALL_MIN, деф. 30) → сигнал-коммент + Needs attention
//      раз за эпизод (потерянный/зависший ход: активный ход рождает сообщения)
// Запуск: systemd --user (uwuwu-factory.service). Тик: 3 минуты.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "../lib/platform"
import { REVIEWER_AGENT_ID } from "../lib/agents"
import { acquireLock, makeDrain, resolveStateDir, sandboxedStateDir } from "../lib/single-instance"
import {
  newUserComments,
  untrackedTasks,
  isUserComment,
  commentText,
  continuePrompt,
  kickoffModeOf,
  spawnPrompt,
  wakeupPrompt,
  bodyEditPrompt,
  isValidTracked,
  reviewKindOf,
  reviewSpawnPrompt,
  reviewRerunPrompt,
  reviewCommentsPrompt,
  reviewReturnPrompt,
  routeTask,
  rowMissingAction,
  worktreeByBranch,
  runBatch,
  parseProjects,
  formatPassSummary,
  stallMinutesFromEnv,
  lastActivityOf,
  stallVerdict,
  adoptBodyDoc,
  pathSafe,
  stampLegacyEntries,
  PATH_CELL,
  DEV_SESSION_CELL,
  REVIEW_SESSION_CELL,
  type PassCounters,
  type ProjectCfg,
  type ReviewKind,
  type FactoryState,
  type KickoffMode,
  type SpawnEnvContext,
  type TaskComment,
  type TaskRef,
  type TrackedTask,
} from "./factory-lib"
import { ensureEnv, cleanupEnv, sweepEnvs, sanitizeRowId, type EnvDeps } from "./env"

// ——— конфиг проектов (только точки входа; пайпы живут в AGENTS.md репо) ———
// ProjectCfg и env-оверрайд (UWUWU_FACTORY_PROJECTS_JSON) — factory-lib;
// здесь только прод-дефолт
export type { ProjectCfg }

const DEFAULT_PROJECTS: Record<string, ProjectCfg> = {
  uwuwu: {
    board: "Задачи uwuwu",
    repo: "yurvon-screamo/uwuwu-agent",
    local: join(homedir(), "uwuwu-agent"),
  },
  origa: {
    board: "Задачи origa",
    repo: "yurvon-screamo/origa",
    local: join(homedir(), "origa"),
  },
  foilcap: {
    board: "Задачи foilcap",
    repo: "git.l4a.ru/ek/ai-generator",
    local: join(homedir(), "foilcap-ai-generator"),
  },
  "true-washing": {
    board: "Задачи true-washing",
    repo: "~/true-washing (detector, saas — репо в подпапках)",
    local: join(homedir(), "true-wash"),
    env: false, // cfg.local — не git-репо: env-контракт невыполним, легаси-поведение
  },
}
// песочница: UWUWU_FACTORY_STATE_DIR двигает state+lock; в песочнице лог
// тоже переезжает (прод-лог не засоряется смоук-тиками)
const STATE_DIR_RAW = process.env.UWUWU_FACTORY_STATE_DIR
const STATE_DIR = resolveStateDir(STATE_DIR_RAW, join(homedir(), ".local/state/uwuwu-factory"))
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = sandboxedStateDir(STATE_DIR_RAW)
  ? join(STATE_DIR, "uwuwu-factory.log")
  : join(homedir(), ".local/share/opencode/log/uwuwu-factory.log")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
const POLL_SEC = Number(process.env.UWUWU_FACTORY_POLL_SEC ?? 180)
/** порог тишины сессии: активный ход рождает сообщения на каждом шаге LLM —
 *  тишина дольше порога = потерянный/зависший ход. Парсер валидирует env
 *  (0/NaN/мусор → дефолт 30), эффективное значение — в стартовый лог. */
const STALL_MIN = stallMinutesFromEnv(process.env.UWUWU_FACTORY_STALL_MIN)
const STALL_MS = STALL_MIN * 60_000
/** разовый тик и выход — для живого смоук-теста */
const ONCE = process.env.UWUWU_FACTORY_ONCE === "1"
/** DRY_RUN: полный цикл решений по реальным доскам (чтение), все мутации
 *  (спавны, промпты, ячейки, комменты, термин сессий) заменяются логом.
 *  Требует песочницу state — гард в main(). */
const DRY_RUN = process.env.UWUWU_FACTORY_DRY_RUN === "1"
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const
/** ревьювер ревью-статусов: праймари-агент, качество важнее цены будильника.
 *  Модель дублирует agents/code-quality-reviewer.md (frontmatter) — править
 *  синхронно: модель сессии спавна главнее для фабричных сессий. */
const REVIEWER = { agent: REVIEWER_AGENT_ID, model: { providerID: "zai-coding-plan", id: "glm-5.3" } } as const
const SPAWN_TIMEOUT_MS = 60_000
const REST_TIMEOUT_MS = 15_000
const MAX_ATTEMPTS = 3
/** параллельность Design-волны: create+prompt+liveness ≈ 5 сетевых шагов
 *  на задачу — 4 дорожки держат волну из ~10 задач в пределах полутика */
const SPAWN_WAVE_CONCURRENCY = 4
/** бот-аккаунт афины: его комменты — не сигналы юзера */
const BOT_USER_ID = process.env.UWUWU_BOT_USER_ID ?? "df415887-98cc-4147-b5df-b6c71d8afeae"
/** корень окружений задач: <root>/<row-id>/{repo,scratch,logs} (на /home,
 *  не tmpfs — урок SIGSEGV 03.11). Песочница переопределяет вместе с state. */
const ENV_ROOT = process.env.UWUWU_FACTORY_ENV_ROOT ?? join(homedir(), "agents-env")
const ENV_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000

/** Исполнитель git-операций окружения: прод — фабричный sh() с таймаутом. */
const envDeps = (): EnvDeps => ({ run: (cmd, cwd) => sh(cmd, { cwd }), log })

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
}

/** Короткий текст ошибки для сводок (лог полного стека — не её дело). */
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function rotateLog(): void {
  try {
    if (!existsSync(LOG_PATH)) return
    if (statSync(LOG_PATH).size > 5 * 1024 * 1024) {
      rmSync(`${LOG_PATH}.old`, { force: true })
      renameSync(LOG_PATH, `${LOG_PATH}.old`)
    }
  } catch {}
}

// ——— одиночный инстанс ———

// onStop лока взводит флаг: проход доезжает до границы, новую работу не спавним
const drain = makeDrain()

// ——— состояние (валидация + атомарная запись) ———

function loadState(): FactoryState {
  try {
    const raw: unknown = JSON.parse(readFileSync(STATE_FILE, "utf-8"))
    const tasks = (raw as { tasks?: unknown }).tasks
    const out: FactoryState = { tasks: {} }
    if (tasks !== null && typeof tasks === "object") {
      for (const [k, v] of Object.entries(tasks as Record<string, unknown>)) {
        if (isValidTracked(v)) {
          out.tasks[k] = v
        } else {
          log(`state: запись ${k.slice(0, 10)} битая — пропущена`)
        }
      }
    }
    // переход на env-механику: записи, спавненные до неё (сессия была, envDir
    // не заводился), доживают в старом поведении — рестарт не уводит их
    // незакоммиченную работу в новый env
    const stamped = stampLegacyEntries(out)
    if (stamped.length > 0) {
      saveState(out)
      log(`переход: штамп легаси — ${stamped.map((id) => id.slice(0, 8)).join(", ")}`)
    }
    return out
  } catch {
    return { tasks: {} }
  }
}

function saveState(state: FactoryState): void {
  mkdirSync(STATE_DIR, { recursive: true })
  if (existsSync(STATE_FILE)) {
    try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
  }
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 2))
  renameSync(tmp, STATE_FILE)
}

// ——— процессы (git/gh/uwuwu-cli) с таймаутом ———

async function sh(cmd: string[], opts: { cwd?: string } = {}): Promise<string> {
  const proc = Bun.spawn({ cmd, cwd: opts.cwd, stdout: "pipe", stderr: "pipe" })
  const killer = setTimeout(() => proc.kill(), SPAWN_TIMEOUT_MS)
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  const code = await proc.exited
  clearTimeout(killer)
  if (code !== 0) throw new Error(`${cmd[0]} (${code}): ${(err.trim() || out.trim()).slice(0, 200)}`)
  return out.trim()
}

// ——— доска (SiYuan database) ———

interface BoardIds { docId: string; dbId: string }

async function findBoard(title: string): Promise<BoardIds> {
  const r = await mcp.tool<{ matches?: { id: string; inTrash?: boolean }[] }>("find_doc_by_title", { title, caseInsensitive: true, limit: 5 })
  let docId = r.matches?.find(m => m.inTrash !== true)?.id
  if (docId === undefined) {
    // индекс тайтлов отстаёт от свежих/переименованных доков — фоллбек
    // через организ-дерево: «Задачи X» → папка X → дети по точному тайтлу
    docId = (await boardViaOrganize(title)) ?? ""
  }
  if (docId === "") throw new Error(`доска «${title}» не найдена`)
  const doc = await mcp.tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
  const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id
  if (dbId === undefined) throw new Error(`в «${title}» нет database-блока`)
  return { docId, dbId }
}

/** Доска без поискового индекса: «Задачи <proj>» → папка <proj> → доки. */
async function boardViaOrganize(title: string): Promise<string | null> {
  const proj = title.startsWith("Задачи ") ? title.slice("Задачи ".length) : null
  if (proj === null) return null
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
  } catch { /* организ недоступен — остаётся ошибка поиска */ }
  return null
}

export async function readTasks(board: BoardIds): Promise<TaskRef[]> {
  const r = await mcp.tool<{
    rows?: {
      rowBlockId?: string
      linkedDocId?: string | null
      title?: string | null
      cells?: Record<string, { value?: string }>
    }[]
  }>("read_database_cells", { docId: board.docId, databaseBlockId: board.dbId })
  const raw = (r.rows ?? []).filter((row) => typeof row.rowBlockId === "string")

  // Фоллбек тела по заголовку для detached-строк (привязки нет) — паритет
  // CLI body_doc_by_title: саб-док доски по hpath «<доска>/<тайтл>».
  // FS-истина (getIDsByHPath) — без лага SQL-индекса. 0 → без тела,
  // 1 → тело, ≥2 → пусто (не угадываем) + агрегированный лог сирот.
  // get_doc доски — один раз на проход (ленивое замыкание: без detached-
  // строк доска не читается вовсе).
  let boardPath: { box: string; hpath: string } | null = null
  const boardPathOnce = async (): Promise<{ box: string; hpath: string }> => {
    if (boardPath === null) {
      const d = await mcp.tool<{ box?: string; hpath?: string }>("get_doc", { docId: board.docId })
      boardPath = { box: d.box ?? "", hpath: d.hpath ?? "" }
    }
    return boardPath
  }
  const orphans: string[] = []
  const rows: TaskRef[] = []
  for (const row of raw) {
    const title = (row.title ?? "").trim()
    let bodyDoc = row.linkedDocId ?? ""
    if (bodyDoc === "" && title !== "") {
      try {
        const bp = await boardPathOnce()
        const found = bp.box === "" || bp.hpath === ""
          ? []
          : (await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", {
              notebook: bp.box, path: `${bp.hpath}/${pathSafe(title)}`,
            })).ids ?? []
        if (found.length === 1) bodyDoc = found[0] as string
        else if (found.length >= 2) orphans.push(`${String(row.rowBlockId).slice(0, 8)} → ${found.join(", ")}`)
      } catch (e) {
        // фоллбек — best effort: сбой ядра на одной detached-строке не должен
        // глушить тик (комменты/будильники здоровых привязанных строк)
        log(`задача ${String(row.rowBlockId).slice(0, 8)}: фоллбек тела по тайтлу не удался (${errText(e)}) — строка без тела`)
      }
    }
    rows.push({
      rowId: row.rowBlockId as string,
      bodyDoc,
      title,
      status: row.cells?.Status?.value ?? "",
      pr: row.cells?.PR?.value ?? "",
      branch: row.cells?.Branch?.value ?? "",
    })
  }
  if (orphans.length > 0) {
    log(`дубль-сироты: ${orphans.length} строк(и) с ≥2 доками одного тайтла — чисти руками в UI: ${orphans.join("; ")}`)
  }
  return rows
}

async function setCells(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<void> {
  if (DRY_RUN) {
    log(`DRY_RUN: setCells ${rowId.slice(0, 8)} ${JSON.stringify(cells)} — пропущен`)
    return
  }
  await mcp.tool("update_database_row", { docId: board.docId, databaseBlockId: board.dbId, rowBlockId: rowId, cells })
}

/** Best-effort запись служебных ячеек (Path/сессии/Branch): колонку владелец
 *  мог не завести — тихий no-op ядра или ошибка не ломают спавн. */
async function setCellsSafe(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<void> {
  try {
    await setCells(board, rowId, cells)
  } catch (e) {
    log(`ячейки ${rowId.slice(0, 8)} (${Object.keys(cells).join(", ")}): ${errText(e)} — продолжаю без записи`)
  }
}

interface CommentEdge { node?: { id?: string; content?: unknown } }

async function commentPage(bodyDoc: string, after: string): Promise<{
  edges: CommentEdge[]
  endCursor: string
  hasNext: boolean
}> {
  const args: Record<string, unknown> = { docId: bodyDoc, first: 50 }
  if (after !== "") args.after = after
  const r = await mcp.tool<{
    edges?: CommentEdge[]
    pageInfo?: { hasNextPage?: boolean; endCursor?: string }
  }>("list_comments", args)
  return {
    edges: r.edges ?? [],
    endCursor: r.pageInfo?.endCursor ?? "",
    hasNext: r.pageInfo?.hasNextPage === true,
  }
}

/** ВСЕ комменты тела с relay-пагинацией (без досылки мост слепнет на
 *  задачах с длинной историей: 50-окно роняет свежие комменты).
 *  Разворачивает нативные треды афины (replies ноды) в плоский список —
 *  reply юзера помечается префиксом «(тред-ответ на «…»)», чтобы агент
 *  видел контекст в ленте; сам ответ агента в тред идёт через
 *  `task comment --reply` (uwuwu-cli). */
async function taskComments(bodyDoc: string): Promise<TaskComment[]> {
  if (bodyDoc === "") return []
  const out: TaskComment[] = []
  let cursor = ""
  for (let page = 0; page < 10; page++) {
    const { edges, endCursor, hasNext } = await commentPage(bodyDoc, cursor)
    for (const e of edges) {
      const n = e.node
      if (n === undefined || typeof n.id !== "string") continue
      const snap = (n.content as { snapshot?: { meta?: { createDate?: unknown } } } | undefined)?.snapshot
      const created = typeof snap?.meta?.createDate === "number" ? snap.meta.createDate : 0
      const text = commentText(n.content)
      const userId = (n as { user?: { id?: unknown } }).user?.id
      out.push({
        id: n.id,
        text,
        createdAt: created,
        ...(typeof userId === "string" ? { userId } : {}),
      })
      // тред-ответы (нативные replies афины): префикс-якорь родителя для контекста
      const replies = (n as { replies?: unknown }).replies
      if (Array.isArray(replies)) {
        const parentHead = text.replace(/^\(в ответ на: [^)]*\)\s*/, "").slice(0, 60)
        for (const rep of replies) {
          const r = rep as { id?: unknown; content?: unknown; createdAt?: unknown; user?: { id?: unknown } }
          if (typeof r.id !== "string") continue
          const repText = commentText(r.content)
          const repUser = r.user?.id
          // GraphQL отдаёт createdAt ISO-строкой; число — только в snapshot
          const repCreated =
            typeof r.createdAt === "string" ? Date.parse(r.createdAt) : created + 1
          out.push({
            id: r.id,
            text: `(тред-ответ на «${parentHead}…») ${repText}`,
            createdAt: Number.isNaN(repCreated) ? created + 1 : repCreated,
            ...(typeof repUser === "string" ? { userId: repUser } : {}),
          })
        }
      }
    }
    if (!hasNext || endCursor === "") break
    cursor = endCursor
  }
  return out
}

async function commentTask(bodyDoc: string, text: string): Promise<boolean> {
  if (bodyDoc === "") return false
  if (DRY_RUN) {
    log(`DRY_RUN: comment → ${bodyDoc.slice(0, 8)}: ${text.slice(0, 80)}`)
    return true
  }
  try {
    await mcp.tool("create_comment", { docId: bodyDoc, content: text })
    return true
  } catch (e) {
    log(`comment: ${e}`)
    return false
  }
}

// ——— opencode REST (service.json) ———

async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = `Basic ${Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")}`
  let res: Response
  try {
    res = await fetch(`${base}/api${path}`, {
      method: init?.method ?? "GET",
      headers: { authorization: auth, "content-type": "application/json" },
      body: init?.body === undefined ? undefined : JSON.stringify(init.body),
      signal: AbortSignal.timeout(REST_TIMEOUT_MS),
    })
  } catch (e) {
    throw Object.assign(new Error(`transport: ${e}`), { status: 0 })
  }
  const text = await res.text()
  if (!res.ok) {
    throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
  }
  if (text.trim() === "") return {}
  const j: unknown = JSON.parse(text)
  return (j as { data?: unknown }).data ?? j
}

interface SessionSpec {
  directory: string
  title: string
  agent?: string
  model?: { providerID: string; id: string }
}

async function createSession(spec: SessionSpec): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: spec.title.replace(/[\r\n]+/g, " ").slice(0, 64),
      model: spec.model ?? MODEL,
      location: { directory: spec.directory },
      ...(spec.agent !== undefined ? { agent: spec.agent } : {}),
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  return typeof id === "string" && id.startsWith("ses_") ? id : null
}

async function promptSession(sessionID: string, text: string): Promise<boolean> {
  if (DRY_RUN) {
    log(`DRY_RUN: prompt → ${sessionID.slice(0, 16)}: ${text.slice(0, 80)}…`)
    return true
  }
  try {
    await restCall(`/session/${sessionID}/prompt`, { method: "POST", body: { text } })
    return true
  } catch (e) {
    log(`prompt ${sessionID}: ${e}`)
    return false
  }
}

type Liveness = "alive" | "dead" | "unknown"

async function sessionLiveness(sessionID: string): Promise<Liveness> {
  if (sessionID === "") return "dead"
  try {
    await restCall(`/session/${sessionID}`)
    return "alive"
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) return "dead"
    return "unknown"
  }
}

/** Последняя активность сессии: max(created, streamed, completed) последнего
 *  сообщения (DESC limit=1). null — сообщений нет; "invalid" — сбой REST или
 *  нераспознанный формат: пропуск тика (транзиентные сбои и смена формата
 *  /message при апгрейде opencode не дают ложных сигналов). */
async function lastSessionActivity(sessionID: string): Promise<number | null | "invalid"> {
  try {
    const raw = await restCall(`/session/${sessionID}/message?limit=1&order=desc`)
    if (!Array.isArray(raw)) return "invalid"
    return lastActivityOf(raw)
  } catch {
    return "invalid"
  }
}

/** updatedAt тела (ISO) или null. */
async function bodyUpdatedAt(bodyDoc: string): Promise<string | null> {
  if (bodyDoc === "") return null
  try {
    const r = await mcp.tool<{ updatedAt?: string }>("get_doc", { docId: bodyDoc })
    return r.updatedAt ?? null
  } catch {
    return null
  }
}

// ——— жизненный цикл задачи ———

/** Вид старта сессии девелопера: свежий спавн (режим от kickoffModeOf)
 *  или рестарт умершей сессии (нейтральный промпт, стадию не предписываем). */
type StartKind = KickoffMode | "continue"

async function startTask(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState,
  attemptsSeed: number, start: StartKind, counters?: PassCounters,
): Promise<void> {
  // стоп-сигнал: не спавним новую работу — задача доедет при следующем старте
  if (drain.isStopping()) return
  // DRY_RUN: решение о спавне принято (routeTask отлогировал) — сессию не создаём
  if (DRY_RUN) {
    log(`DRY_RUN: спавн девелопера над ${task.rowId.slice(0, 8)} (${start}) — пропущен`)
    return
  }
  // env-механика: work/task-ветку и ворктри делает фабрика, сессия стартует
  // cwd=repo. Легаси-штамп или env:false-проект → старое поведение (агент сам)
  const prev = state.tasks[task.rowId]
  const useEnv = cfg.env !== false && prev?.legacy !== true
  let envCtx: SpawnEnvContext | undefined
  let envDir: string | undefined
  if (useEnv) {
    const paths = await ensureEnv(envDeps(), { root: ENV_ROOT, rowId: task.rowId, local: cfg.local, base: "origin/master" })
    envCtx = { repoDir: paths.repoDir, branch: paths.branch, scratchDir: paths.scratchDir }
    envDir = paths.envDir
    if (counters !== undefined) counters.envCreated++
  }

  // тайтл: линкованная строка несёт заголовок в доке-теле — деривируем из него
  let title = task.title
  if (title === "" && task.bodyDoc !== "") {
    const doc = await mcp.tool<{ title?: string }>("get_doc", { docId: task.bodyDoc })
    title = (doc.title ?? "").trim()
  }
  if (title === "") title = `задача ${task.rowId.slice(0, 8)}`

  // имя сессии «проект: тайтл» — осмысленно в списке сессий опенкода
  const sessionID = await createSession({
    directory: envCtx?.repoDir ?? cfg.local,
    title: `📋 ${projectName}: ${title}`,
  })
  if (sessionID === null) throw new Error("сессия не создалась")

  const sent = await promptSession(
    sessionID,
    start === "continue"
      ? continuePrompt(task.rowId, projectName, cfg.repo, envCtx)
      : spawnPrompt(task.rowId, projectName, cfg.repo, start, envCtx),
  )
  if (!sent) throw new Error("spawn-промпт не ушёл")

  // живость сразу после спавна: «фиктивные sessionID» (create ок, сессии
  // нет) не попадают в state и не считаются активными — throw уводит в
  // registerFailure, ретрай следующим тиком; осиротевшую сессию гасим
  const liveness = await sessionLiveness(sessionID)
  if (liveness === "dead") {
    await restCall(`/session/${sessionID}`, { method: "DELETE" }).catch(() => {})
    throw new Error("сессия мертва сразу после спавна")
  }
  // unknown (транзиент REST) — пишем state как раньше: stage-2 перепроверит

  // граница комментов ДО старта — не будить агента старыми;
  // маркер = последний юзерский коммент (позиционный поиск в
  // newUserComments идёт по юзерскому списку)
  const comments = (await taskComments(task.bodyDoc)).filter((c) =>
    isUserComment(c, BOT_USER_ID),
  )
  const last = comments.length > 0
    ? comments.reduce((m, c) => (c.createdAt >= m.createdAt ? c : m), comments[0])
    : null

  state.tasks[task.rowId] = {
    sessionID,
    worktree: envCtx?.repoDir ?? "",
    branch: envCtx?.branch ?? "",
    bodyDoc: task.bodyDoc,
    title,
    lastCommentAt: last?.createdAt ?? 0,
    lastCommentId: last?.id ?? "",
    createdAt: Date.now(),
    attempts: attemptsSeed,
    project: projectName,
    bodyUpdated: await bodyUpdatedAt(task.bodyDoc) ?? undefined,
    agentActive: true, // spawn отправлен — агент работает
    lastStatus: task.status, // спавн из этого статуса (Design/In Progress) — не рестартить им же
    ...(envDir !== undefined ? { envDir } : {}),
    ...(prev?.legacy === true ? { legacy: true } : {}),
  }
  saveState(state)
  await setCellsSafe(board, task.rowId, {
    ...(envCtx !== undefined
      ? { Branch: envCtx.branch, [PATH_CELL]: envCtx.repoDir }
      : {}),
    [DEV_SESSION_CELL]: sessionID,
  })
  log(`задача ${task.rowId.slice(0, 8)}: сессия ${sessionID}, старт ${start}${envCtx !== undefined ? `, env=${envDir}` : " (легаси-поведение)"} (попыток до этого: ${attemptsSeed})`)
}

/** Чистка state (Done): гасим ОБЕ сессии задачи и сносим env (rm/attic +
 *  prune + branch -d). Живой дев-ход умирает вместе с задачей — Done
 *  означает мяч у владельца; данные при незакоммиченном — в attic. */
async function finishTask(
  rowId: string, tracked: TrackedTask, cfg: ProjectCfg, state: FactoryState, counters?: PassCounters,
): Promise<void> {
  // DRY_RUN: чистка в песочный state безвредна, но термин реальных сессий
  // и снос реального env — side-effect на прод-сервере — не делаем
  if (DRY_RUN) {
    log(`DRY_RUN: Done-чистка ${rowId.slice(0, 8)} — state-запись удалена бы, сессии не терминируются, env не сносится`)
    delete state.tasks[rowId]
    saveState(state)
    return
  }
  log(`задача ${rowId.slice(0, 8)}: Done — чистка state`)
  if (tracked.sessionID !== "") {
    await restCall(`/session/${tracked.sessionID}`, { method: "DELETE" }).catch(() => {})
  }
  if (tracked.reviewSessionID !== undefined && tracked.reviewSessionID !== "") {
    await restCall(`/session/${tracked.reviewSessionID}`, { method: "DELETE" }).catch(() => {})
  }
  const hasEnv = tracked.envDir !== undefined || tracked.worktree !== ""
  if (hasEnv) {
    const outcome = await cleanupEnv(envDeps(), {
      root: ENV_ROOT, rowId, local: cfg.local,
      envDir: tracked.envDir, branch: tracked.branch !== "" ? tracked.branch : undefined,
    })
    if (counters !== undefined) {
      if (outcome === "removed") counters.envRemoved++
      else if (outcome === "atticked") counters.envAtticked++
    }
    if (outcome === "atticked") {
      await notify(tracked,
        `[factory] Задача закрыта, но в окружении осталось незакоммиченное — env переехал в attic (${ENV_ROOT}/.attic), тяжёлые каталоги вырезаны. Разобрать или дослить — руками.`)
    } else if (outcome === "failed") {
      await notify(tracked,
        `[factory] Задача закрыта, но снос окружения не удался — каталог оставлен как есть, разбери ${ENV_ROOT} руками.`)
    }
  }
  delete state.tasks[rowId]
  saveState(state)
}

async function notify(task: TrackedTask, body: string): Promise<void> {
  // один ретрай: потеря коммента недопустима
  if (await commentTask(task.bodyDoc, body)) return
  await new Promise((r) => setTimeout(r, 15_000))
  if (!(await commentTask(task.bodyDoc, body))) log(`уведомление не доставлено: ${body.slice(0, 80)}`)
}

async function registerFailure(
  task: TaskRef, projectName: string, board: BoardIds, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  const prev = state.tasks[task.rowId]
  const entry: TrackedTask = prev ?? {
    sessionID: "", worktree: "", branch: "",
    bodyDoc: task.bodyDoc, title: task.title, project: projectName,
    lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
  }
  // без этого чужой тик считал бы запись своей потерянной («строка исчезла»)
  // и стирал её — бесконечный цикл стартов вместо Blocked
  if (entry.project === "") entry.project = projectName
  entry.attempts = attempts
  state.tasks[task.rowId] = entry
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    // единый статус «ждёт юзера»: не важно — агент вопрос задал или
    // демон не смог стартовать; реакция юзера одна (коммент в задачу)
    await setCells(board, task.rowId, { Status: "Needs attention" })
    await notify(entry,
      `[factory] не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH} и напиши комментарий — попробую снова.`)
  }
}

async function restartTask(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState, fresh: TaskComment[],
): Promise<void> {
  const tracked = state.tasks[task.rowId]
  if (tracked === undefined) return
  const seed = tracked.attempts
  const last = fresh[fresh.length - 1]
  tracked.lastCommentAt = last.createdAt
  tracked.lastCommentId = last.id
  tracked.sessionID = "" // карантин до успеха старта
  // ревью-сессию переживаем рестартом девелопера (1 задача = 1 ревьювер)
  const savedReview = tracked.reviewSessionID
  const savedReviewAttempts = tracked.reviewAttempts
  saveState(state)
  try {
    await startTask(task, projectName, cfg, board, state, seed, "continue")
    const reborn = state.tasks[task.rowId]
    if (reborn !== undefined && savedReview !== undefined) {
      reborn.reviewSessionID = savedReview
      reborn.reviewAttempts = savedReviewAttempts
      saveState(state)
    }
  } catch (e) {
    await registerFailure(task, projectName, board, state, seed, e)
  }
}

// ——— ревьювер: спавн/будильник на ревью-статусах ———

/** Ворктри задачи по Branch-ячейке (легаси-fallback перехода: env-задачи
 *  берут repoDir из state). */
async function resolveWorktree(local: string, branch: string): Promise<string> {
  if (branch === "") return ""
  try {
    return worktreeByBranch(await sh(["git", "-C", local, "worktree", "list", "--porcelain"]), branch)
  } catch { /* git недоступен — ревьюверу останется cfg.local/PR */ }
  return ""
}

/** Спавн ревьювера: 1 сессия на задачу. Без parentID: наследование каталога
 *  родителем побеждает явный location (проверено на opencode REST) — а ревьюверу
 *  нужен cwd ворктри. Выбрасывает при фейле. */
async function startReview(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState,
  kind: ReviewKind, tracked: TrackedTask,
): Promise<void> {
  // DRY_RUN: решение о ревью принято — ревьювера не спавним
  if (DRY_RUN) {
    log(`DRY_RUN: спавн ревьювера (${kind}) над ${task.rowId.slice(0, 8)} — пропущен`)
    return
  }
  // env-задача: repoDir из state (оба вида ревью — код живёт там);
  // легаси-fallback: resolveWorktree по Branch-ячейке (переход)
  const worktree = tracked.worktree !== ""
    ? tracked.worktree
    : kind === "result" ? await resolveWorktree(cfg.local, task.branch) : ""
  const ctx = { branch: task.branch, worktree, pr: task.pr }
  const sessionID = await createSession({
    directory: worktree !== "" ? worktree : cfg.local,
    title: `🔍 ${projectName}: ревью — ${tracked.title || task.title}`,
    agent: REVIEWER.agent,
    model: REVIEWER.model,
  })
  if (sessionID === null) throw new Error("ревью-сессия не создалась")
  const sent = await promptSession(sessionID, reviewSpawnPrompt(task.rowId, projectName, kind, ctx))
  if (!sent) {
    await restCall(`/session/${sessionID}`, { method: "DELETE" }).catch(() => {})
    throw new Error("ревью-промпт не ушёл")
  }
  tracked.reviewSessionID = sessionID
  tracked.reviewAttempts = 0
  saveState(state)
  await setCellsSafe(board, task.rowId, { [REVIEW_SESSION_CELL]: sessionID })
  log(`задача ${task.rowId.slice(0, 8)}: ревью-сессия ${sessionID} (${kind})`)
}

/** Эскалация фейлов спавна ревьювера: 3 подряд → Needs attention. */
async function failReview(
  task: TaskRef, board: BoardIds, tracked: TrackedTask, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  tracked.reviewAttempts = attempts
  tracked.reviewSessionID = ""
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: ревью-старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    await setCells(board, task.rowId, { Status: "Needs attention" })
    // lastStatus — сразу за доской: если юзер переставит ревью-статус быстрее
    // тика, entering-проверка всё равно посчитает это новым входом (seed=0)
    tracked.lastStatus = "Needs attention"
    saveState(state)
    await notify(tracked,
      `[factory] ревьювер не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH}; ` +
      "чтобы перезапустить ревью — переставь ревью-статус (счётчик попыток обнулится).")
  }
}

interface ReviewDeps {
  row: TaskRef
  tracked: TrackedTask
  project: string
  cfg: ProjectCfg
  board: BoardIds
  state: FactoryState
  fresh: TaskComment[]
  consumeComments: () => void
  refreshBodyBaseline: () => Promise<void>
}

/** Исполнение решения routeTask. Возвращает "skip-rest", когда этот тик по
 *  задаче закончен целиком (рестарт девелопера — его запись state заменена). */
async function dispatchReviewAction(
  action: ReturnType<typeof routeTask>, d: ReviewDeps,
): Promise<"skip-rest" | undefined> {
  const shortId = d.row.rowId.slice(0, 8)
  switch (action.t) {
    case "spawn-reviewer": {
      // новый вход в ревью-статус = свежая попытка (сброс счётчика фейлов):
      // юзер/дев переустановили статус после Needs attention
      const entering = reviewKindOf(d.tracked.lastStatus ?? "") !== action.kind
      const seed = entering ? 0 : d.tracked.reviewAttempts ?? 0
      try {
        await startReview(d.row, d.project, d.cfg, d.board, d.state, action.kind, d.tracked)
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } catch (e) {
        await failReview(d.row, d.board, d.tracked, d.state, seed, e)
      }
      return undefined
    }
    case "rerun-reviewer": {
      const worktree = d.tracked.worktree !== ""
        ? d.tracked.worktree
        : action.kind === "result" ? await resolveWorktree(d.cfg.local, d.row.branch) : ""
      const ok = await promptSession(
        d.tracked.reviewSessionID ?? "",
        reviewRerunPrompt(d.row.rowId, action.kind, { branch: d.row.branch, worktree, pr: d.row.pr }),
      )
      if (ok) {
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: доревью-промпт не ушёл — следующий тик перепроверит сессию`)
      }
      return undefined
    }
    case "comments-to-reviewer": {
      const ok = await promptSession(d.tracked.reviewSessionID ?? "", reviewCommentsPrompt(d.row.rowId, d.fresh))
      if (ok) {
        d.consumeComments()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: комменты ревьюверу не ушли — граница не сдвинута`)
      }
      return undefined
    }
    case "return-to-dev": {
      const devLiveness = d.tracked.sessionID === "" ? "dead" : await sessionLiveness(d.tracked.sessionID)
      if (devLiveness === "alive") {
        const ok = await promptSession(d.tracked.sessionID, reviewReturnPrompt(d.row.rowId))
        if (!ok) log(`задача ${shortId}: возврат из ревью — промпт девелоперу не ушёл`)
        return undefined
      }
      if (devLiveness === "unknown") {
        // транзиентная ошибка REST — не плодим дубли сессий, следующий тик перепроверит
        log(`задача ${shortId}: возврат из ревью, живость девелопера неизвестна — пропуск`)
        return undefined
      }
      // сессия девелопера потеряна — свежий старт (контекст живёт в задаче),
      // ревью-сессию сохраняем: инвариант 1 задача = 1 ревьювер.
      // startTask заменяет запись state — дальше по циклу не идём:
      // свежая сессия уже получила рестарт-промпт (стадию не предписываем)
      const savedReview = d.tracked.reviewSessionID
      const savedReviewAttempts = d.tracked.reviewAttempts
      try {
        await startTask(d.row, d.project, d.cfg, d.board, d.state, d.tracked.attempts, "continue")
        const reborn = d.state.tasks[d.row.rowId]
        if (reborn !== undefined && savedReview !== undefined) {
          reborn.reviewSessionID = savedReview
          reborn.reviewAttempts = savedReviewAttempts
          saveState(d.state)
        }
        log(`задача ${shortId}: возврат из ревью при мёртвом девелопере — рестарт сессии`)
      } catch (e) {
        await registerFailure(d.row, d.project, d.board, d.state, d.tracked.attempts, e)
      }
      return "skip-rest"
    }
    case "noop":
      return undefined
  }
}

export async function tickProject(state: FactoryState, projectName: string, cfg: ProjectCfg): Promise<void> {
  const board = await findBoard(cfg.board)
  const rows = await readTasks(board)
  // наблюдаемость полноты скана: N сверяемо со счётчиком ядра (view.rowCount)
  log(`проход ${projectName}: строк ${rows.length}`)
  const counters: PassCounters = {
    devSpawned: 0, reviewSpawned: 0, wakeups: 0, cleanups: 0,
    envCreated: 0, envRemoved: 0, envAtticked: 0,
    watched: 0, failures: [],
  }

  // 1. новые задачи: Design → девелопер (утряска), In Progress без state →
  //    девелопер (fast-track владельца: сразу реализация), ревью-статус → ревьювер
  //    (Draft = черновик триажа, пустой = легаси — оба не спавнят)
  const started = new Set<string>()
  const spawnable = rows.filter(
    (r) => r.status === "Design" || r.status === "In Progress" || reviewKindOf(r.status) !== null)
  const untracked = untrackedTasks(spawnable, state)

  // дев-волна — пачкой параллельно: все ненатрекенные задачи (Design-утряска
  // и fast-track In Progress владельца) получают сессии за ОДИН проход
  // (прод-кейс 05.11: последовательный спавн и слепота страницы 1
  // растягивали волну из 4 задач на 12+ минут)
  const devWave = untracked.filter((t) => reviewKindOf(t.status) === null)
  if (devWave.length > 0 && !drain.isStopping()) {
    await runBatch(devWave, async (task) => {
      if (drain.isStopping()) return // стоп в середине пачки — доедет следующий старт
      try {
        await startTask(task, projectName, cfg, board, state, 0, kickoffModeOf(task.status), counters)
        // инвариант против drain-отмены внутри startTask (молчаливый return):
        // сессия есть ⇔ в state появилась запись — иначе спавном не считаем
        if (state.tasks[task.rowId] === undefined) return
        started.add(task.rowId)
        counters.devSpawned++
      } catch (e) {
        await registerFailure(task, projectName, board, state, 0, e)
        counters.failures.push(`${task.rowId.slice(0, 8)}: ${errText(e)}`)
      }
    }, SPAWN_WAVE_CONCURRENCY)
  }

  for (const task of untracked.filter((t) => reviewKindOf(t.status) !== null)) {
    const kind = reviewKindOf(task.status)
    if (kind === null) continue // защищённость от перефильтра выше
    // ревью-статус без state (даунтайм демона при переходе) — карантинная
    // запись (девелопер неизвестен) + спавн ревьювера; lastStatus = текущий,
    // чтобы следующий тик не принял спавн за новый вход (паразитный rerun)
    const comments = (await taskComments(task.bodyDoc)).filter((c) => isUserComment(c, BOT_USER_ID))
    const last = comments.length > 0
      ? comments.reduce((m, c) => (c.createdAt >= m.createdAt ? c : m), comments[0])
      : null
    const entry: TrackedTask = {
      sessionID: "", worktree: "", branch: "",
      bodyDoc: task.bodyDoc, title: task.title, project: projectName,
      lastCommentAt: last?.createdAt ?? 0, lastCommentId: last?.id ?? "",
      createdAt: Date.now(), attempts: 0, reviewAttempts: 0,
      lastStatus: task.status,
      bodyUpdated: (await bodyUpdatedAt(task.bodyDoc)) ?? undefined,
    }
    state.tasks[task.rowId] = entry
    try {
      await startReview(task, projectName, cfg, board, state, kind, entry)
      counters.reviewSpawned++
    } catch (e) {
      await failReview(task, board, entry, state, 0, e)
      counters.failures.push(`${task.rowId.slice(0, 8)}: ${errText(e)}`)
    }
    started.add(task.rowId)
  }

  // 2. активные этой доски (state общий на все проекты)
  for (const rowId of Object.keys(state.tasks)) {
    const tracked = state.tasks[rowId]
    if ((tracked.project || "uwuwu") !== projectName) continue
    if (started.has(rowId)) continue // стартован этим тиком — rows устарели
    const row = rows.find((r) => r.rowId === rowId)
    // suspect-механика: необратимую чистку (finishTask = delete state +
    // DELETE ревью-сессии) страхуем от одиночного промаха скана — строка
    // должна отсутствовать в двух проходах подряд
    const missing = rowMissingAction(tracked.missingSince, row !== undefined)
    if (missing === "clear") {
      delete tracked.missingSince
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: строка вернулась в скан — suspect-пометка снята`)
    } else if (missing === "mark") {
      tracked.missingSince = new Date().toISOString()
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: строки нет в скане — suspect, чистка не раньше следующего прохода`)
    } else if (missing === "clean") {
      log(`задача ${rowId.slice(0, 8)}: строки нет в скане 2 прохода подряд (с ${tracked.missingSince}) — чистка`)
      await finishTask(rowId, tracked, cfg, state, counters)
      counters.cleanups++
      continue
    }
    if (row === undefined) continue // mark: маршрутизировать нечего

    // ——— самоисцеление: adopt + базлайн тела (до маршрутизации/комментов) ———
    // привязка/фоллбек в state пусты → принять из свежего скана; без засева
    // bodyUpdated канал body-edit-wakeup (ниже требует непустого базлайна)
    // оставался бы мёртвым у исцелённых треков
    const adopted = adoptBodyDoc(tracked, row)
    let healed = false
    if (adopted !== null) {
      tracked.bodyDoc = adopted
      healed = true
      log(`задача ${rowId.slice(0, 8)}: пустой bodyDoc трека — принята привязка строки ${adopted.slice(0, 8)}`)
    }
    if (tracked.bodyDoc !== "" && (tracked.bodyUpdated ?? "") === "") {
      const seeded = await bodyUpdatedAt(tracked.bodyDoc)
      if (seeded !== null) {
        tracked.bodyUpdated = seeded
        healed = true
      }
    }
    if (healed) saveState(state)

    // Done (мерж-крон агента, Spike-агент, руки юзера) → чистка ворктри
    if (row.status === "Done") {
      log(`задача ${rowId.slice(0, 8)}: Done (закрыта агентом/юзером) — чистка`)
      await finishTask(rowId, tracked, cfg, state, counters)
      counters.cleanups++
      continue
    }

    // ——— маршрутизация: решение ДО перезаписи lastStatus и карантинных веток ———
    const comments = await taskComments(tracked.bodyDoc)
    const fresh = newUserComments(comments, { at: tracked.lastCommentAt, id: tracked.lastCommentId }, BOT_USER_ID)
    const reviewerLiveness = tracked.reviewSessionID === undefined || tracked.reviewSessionID === ""
      ? "dead"
      : await sessionLiveness(tracked.reviewSessionID)
    const action = routeTask({
      status: row.status,
      lastStatus: tracked.lastStatus ?? "",
      reviewerAlive: reviewerLiveness === "alive" ? true : reviewerLiveness === "dead" ? false : null,
      freshComments: fresh,
    })
    let commentsConsumed = false
    const consumeComments = (): void => {
      const lastComment = fresh[fresh.length - 1]
      if (lastComment === undefined) return
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      commentsConsumed = true
    }
    /** базлайн тела: дев правил тело перед передачей мяча — этим не будить */
    const refreshBodyBaseline = async (): Promise<void> => {
      const updated = await bodyUpdatedAt(tracked.bodyDoc)
      if (updated !== null) tracked.bodyUpdated = updated
    }

    const skipRest = await dispatchReviewAction(action, {
      row, tracked, project: tracked.project || projectName, cfg, board, state, fresh,
      consumeComments, refreshBodyBaseline,
    })
    if (action.t !== "noop") counters.wakeups++ // спавн/реран/комменты/возврат — будильники ревью-контура
    if (skipRest === "skip-rest") continue

    // запоминаем наблюдённый статус — граница перехода для след. тика
    if (tracked.lastStatus !== row.status) {
      tracked.lastStatus = row.status
      // смена статуса = новый эпизод застоя: пере-вход в In Progress должен
      // уметь сигналить снова (наш собственный сигнал уводит строку из
      // In Progress — без сброса флаг заглушил бы повторные застои навсегда)
      if (tracked.stallSignaled === true) {
        delete tracked.stallSignaled
        log(`задача ${rowId.slice(0, 8)}: статус сменился — флаг застоя сброшен (новый эпизод)`)
      }
      saveState(state)
    }

    // ——— пайп девелопера: комменты/тело/живость (комменты мог съесть ревьювер) ———
    const devFresh = commentsConsumed ? [] : fresh

    // будильник правок ТЕЛА без комментов — до карантинных веток: адресат
    // по статусу (в ревью — ревьюверу, даже если девелопер в карантине)
    if (devFresh.length === 0) {
      const updated = await bodyUpdatedAt(tracked.bodyDoc)
      const bodyChanged = updated !== null
        && (tracked.bodyUpdated ?? "") !== ""
        && updated > (tracked.bodyUpdated ?? "")
      if (bodyChanged) {
        const inReview = reviewKindOf(row.status) !== null && (tracked.reviewSessionID ?? "") !== ""
        const target = inReview ? tracked.reviewSessionID ?? "" : tracked.sessionID
        if (target !== "") {
          log(`задача ${rowId.slice(0, 8)}: тело менялось → wakeup немедленно (${inReview ? "ревьювер" : "девелопер"})`)
          if (await promptSession(target, bodyEditPrompt(rowId))) {
            tracked.bodyUpdated = updated
            saveState(state)
            counters.wakeups++
          }
          continue
        }
        // таргета нет (дев в карантине, ревью-сессии тоже) — базлайн не двигаем:
        // добудим адресата на следующем тике
      }
    }

    if (tracked.sessionID === "") {
      // карантин: ждём явной активности юзера
      if (devFresh.length === 0) continue
      await restartTask(row, tracked.project || projectName, cfg, board, state, devFresh)
      continue
    }

    if (devFresh.length === 0) {
      // юзер молчит, но сессия могла умереть посреди прогона
      const quiet = await sessionLiveness(tracked.sessionID)
      if (quiet === "dead") {
        log(`задача ${rowId.slice(0, 8)}: сессия умерла (юзер молчит) — карантин + уведомление`)
        tracked.sessionID = ""
        saveState(state)
        await notify(tracked,
          "[factory] сессия разработки потеряна. Напиши комментарий — перезапущу (несохранённое в ворктри будет перезаписано с master).")
      } else if (quiet === "alive" && row.status === "In Progress") {
        // сессия жива — ловим зависший ход: агент молчит дольше порога
        // (активный ход рождает сообщения на каждом шаге LLM). Неизвестная
        // активность (REST/формат) — пропуск тика без операций с флагом
        const activity = await lastSessionActivity(tracked.sessionID)
        if (activity === "invalid") {
          log(`задача ${rowId.slice(0, 8)}: активность сессии неизвестна — пропуск тика`)
        } else {
          // git только когда вердикт может стать signal: предварительный чек
          // застоя — решение остаётся в stallVerdict, ветка в facts нужна
          // лишь для текста сигнала
          const stalled = activity === null || Date.now() - activity > STALL_MS
          const branchInWorktree = stalled &&
            (tracked.worktree !== "" || (await resolveWorktree(cfg.local, row.branch)) !== "")
          const verdict = stallVerdict({
            status: row.status,
            sessionAlive: true,
            lastActivityAt: activity,
            branch: row.branch,
            branchInWorktree,
            signaled: tracked.stallSignaled === true,
            now: Date.now(),
          }, STALL_MS)
          if (verdict.t === "signal") {
            // эскалация как у registerFailure/failReview: коммент + Needs
            // attention; lastStatus сразу за доской (как в failReview)
            log(`задача ${rowId.slice(0, 8)}: In Progress при живой сессии, тишина > ${STALL_MIN} мин — сигнал + Needs attention`)
            if (DRY_RUN) {
              // setCells/notify в DRY_RUN сами глухие, а state-мутации не пишем:
              // без флага анти-спам в песочнице не работает — сигнал повторялся
              // бы каждые 2 тика (расхождение с продом задокументировано логом)
              log(`DRY_RUN: эскалация Needs attention + коммент — пропущены`)
            } else {
              await setCells(board, rowId, { Status: "Needs attention" })
              tracked.stallSignaled = true
              tracked.lastStatus = "Needs attention"
              saveState(state)
              await notify(tracked, verdict.text)
            }
          } else if (verdict.t === "none" && tracked.stallSignaled === true) {
            delete tracked.stallSignaled
            saveState(state)
            // до этой ветки штатный путь «возврат в In Progress» не доходит —
            // флаг снимает сброс на смене статуса выше; здесь — эпизод,
            // завершённый без смены статуса (наш сигнал поставил Needs attention,
            // rollback-окно старого кода), формулировка обобщённая
            log(`задача ${rowId.slice(0, 8)}: эпизод застоя завершён — флаг снят (статус вне In Progress или активность вернулась)`)
          }
        }
      }
      continue
    }

    const liveness = await sessionLiveness(tracked.sessionID)
    if (liveness === "unknown") {
      log(`задача ${rowId.slice(0, 8)}: живость сессии неизвестна — пропускаю тик`)
      continue
    }
    if (liveness === "dead") {
      log(`задача ${rowId.slice(0, 8)}: сессия умерла — перезапуск`)
      await restartTask(row, tracked.project || projectName, cfg, board, state, devFresh)
      continue
    }

    // правка ТЕЛА при наличии комментов: wakeup-комменты ниже разбудят той
    // же пачкой — тело агент перечитает сам, двигаем только базлайн
    const updatedWithComments = await bodyUpdatedAt(tracked.bodyDoc)
    if (
      updatedWithComments !== null &&
      (tracked.bodyUpdated ?? "") !== "" &&
      updatedWithComments > (tracked.bodyUpdated ?? "")
    ) {
      tracked.bodyUpdated = updatedWithComments
      saveState(state)
    }

    log(`задача ${rowId.slice(0, 8)}: ${devFresh.length} новых коммент(ов) → сессия`)
    if (await promptSession(tracked.sessionID, wakeupPrompt(rowId, devFresh))) {
      const lastComment = devFresh[devFresh.length - 1]
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      tracked.agentActive = true
      saveState(state)
      counters.wakeups++
    } else {
      log(`задача ${rowId.slice(0, 8)}: промпт не ушёл — граница комментов не сдвинута`)
    }
  }

  // сводка прохода: «молчаливые тики» видны — что сделано и сколько живых
  counters.watched = Object.values(state.tasks)
    .filter((t) => (t.project || "uwuwu") === projectName).length
  log(formatPassSummary(projectName, counters))
}

let tickCount = 0

async function main(): Promise<void> {
  // env шлюза обязателен на старте (раньше это делал process.exit на импорте
  // mcp.ts — теперь проверка явная, модуль шарится с ленивой конфигурацией)
  if (!mcp.mcpConfigured()) {
    console.error("mcp: нужен UWUWU_SIYUAN_TOKEN (или UWUWU_SIYUAN_WS)")
    process.exit(1)
  }
  // env-оверрайд проектов: валидируем ДО лока — невалидный конфиг не должен
  // даже притворяться демоном (тестовые доски для ONCE-прогонов)
  let projects: Record<string, ProjectCfg>
  try {
    projects = parseProjects(process.env.UWUWU_FACTORY_PROJECTS_JSON, DEFAULT_PROJECTS)
  } catch (e) {
    console.error(errText(e))
    process.exit(1)
  }
  // гард dry-run ДО лока: невалидный режим не должен даже притворяться
  // демоном (фиктивные записи не должны попадать в прод-state)
  if (DRY_RUN && !sandboxedStateDir(STATE_DIR_RAW)) {
    console.error("DRY_RUN требует UWUWU_FACTORY_STATE_DIR (песочница state) — фиктивные записи не должны попадать в прод-state")
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: drain.stop })) {
    console.error("другой живой инстанс uwuwu-factory держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  rotateLog()
  await mcp.connect()
  log(`uwuwu-factory: старт (проекты: ${Object.keys(projects).join(", ")}, poll=${POLL_SEC}s, stall=${STALL_MIN}m, model=${MODEL.id}${DRY_RUN ? ", DRY_RUN" : ""}${sandboxedStateDir(STATE_DIR_RAW) ? `, state=${STATE_DIR}` : ""})`)
  const state = loadState()
  log(`восстановлено задач: ${Object.keys(state.tasks).length}`)
  // старт-свип окружений: сироты без записи в state и старый attic — только
  // прод (песочница/DRY_RUN не трогают реальный ENV_ROOT)
  if (!DRY_RUN && !sandboxedStateDir(STATE_DIR_RAW)) {
    const sweep = sweepEnvs(envDeps(), {
      root: ENV_ROOT,
      // имена каталогов прошли sanitizeRowId (envPaths) — сравниваем в том же
      // нормализованном виде (изменится формат id — живой env не снесётся)
      activeRowIds: new Set(Object.keys(state.tasks).map(sanitizeRowId)),
      nowMs: Date.now(),
      maxAgeMs: ENV_MAX_AGE_MS,
    })
    log(`старт-свип env (${ENV_ROOT}): снесено сирот=${sweep.sweptEnvs.length}, attic=${sweep.sweptAttic.length}`)
  }
  while (true) {
    for (const [name, cfg] of Object.entries(projects)) {
      if (drain.isStopping()) break
      try {
        await tickProject(state, name, cfg)
      } catch (e) {
        log(`tick[${name}]: ${e}`)
      }
    }
    tickCount++
    if (tickCount % 10 === 0) rotateLog()
    if (ONCE) {
      log("ONCE-режим: тик выполнен — выходим")
      process.exit(0)
    }
    if (drain.isStopping()) {
      log("стоп-сигнал: проход доехал — выходим")
      process.exit(0)
    }
    await drain.sleep(POLL_SEC * 1000)
  }
}

if (import.meta.main) {
  void main()
}
