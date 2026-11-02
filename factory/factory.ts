// uwuwu-factory — будильник фабрики разработки + маршрутизатор ревью на AFFiNE.
// Вся логика пайпа — у агентов (глобальный AGENTS.md, раздел «Работа над
// задачей»). Демон делает четыре вещи:
//   1. Design на доске (пустой статус = черновик юзера, не трогаем) →
//      сессия девелопера в чекауте репо + короткий spawn-промпт (ворктри/ветку
//      создаёт сам агент)
//   2. «Ревью плана»/«Ревью результата» → спавн/будильник ревьювера
//      (праймари code-quality-reviewer, 1 сессия на задачу, cwd = ворктри
//      задачи; от дримера исключён фильтром по agent); возврат статуса
//      из ревью → wakeup девелопера; комменты юзера в ревью-статусах —
//      ревьюверу
//   3. новые комменты/правки тела юзера → wakeup сессии (мёртвая → карантин,
//      коммент юзера → рестарт)
//   4. Done (кто бы ни поставил — мерж-крон агента или юзер) → чистка
//      state + термин живой ревью-сессии
// Запуск: systemd --user (uwuwu-factory.service). Тик: 3 минуты.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "./mcp"
import { acquireLock, makeDrain } from "../lib/single-instance"
import {
  newUserComments,
  untrackedTasks,
  isUserComment,
  commentText,
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
  type ReviewKind,
  type FactoryState,
  type TaskComment,
  type TaskRef,
  type TrackedTask,
} from "./factory-lib"

// ——— конфиг проектов (только точки входа; пайпы живут в AGENTS.md репо) ———
export interface ProjectCfg {
  board: string
  repo: string
  local: string
}

const PROJECTS: Record<string, ProjectCfg> = {
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
  },
}
const STATE_DIR = join(homedir(), ".local/state/uwuwu-factory")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-factory.log")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
const POLL_SEC = Number(process.env.UWUWU_FACTORY_POLL_SEC ?? 180)
/** разовый тик и выход — для живого смоук-теста */
const ONCE = process.env.UWUWU_FACTORY_ONCE === "1"
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const
/** ревьювер ревью-статусов: праймари-агент, качество важнее цены будильника */
const REVIEWER = { agent: "code-quality-reviewer", model: { providerID: "zai-coding-plan", id: "glm-5.3" } } as const
const SPAWN_TIMEOUT_MS = 60_000
const REST_TIMEOUT_MS = 15_000
const MAX_ATTEMPTS = 3
/** бот-аккаунт афины: его комменты — не сигналы юзера */
const BOT_USER_ID = process.env.UWUWU_BOT_USER_ID ?? "df415887-98cc-4147-b5df-b6c71d8afeae"

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
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

// ——— доска AFFiNE ———

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
  const dbId = doc.blocks?.find((b) => b.flavour === "affine:database" || b.flavour === "affine:data_view")?.id
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

async function readTasks(board: BoardIds): Promise<TaskRef[]> {
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
      status: row.cells?.Status?.value ?? "",
      pr: row.cells?.PR?.value ?? "",
      branch: row.cells?.Branch?.value ?? "",
    }))
}

async function setCells(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<void> {
  await mcp.tool("update_database_row", { docId: board.docId, databaseBlockId: board.dbId, rowBlockId: rowId, cells })
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
 *  Разворачивает и треды (replies ноды) — ответ юзера тредом виден
 *  с якорем «тред-ответ на …». */
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
      // тред-ответы: отдельные комменты с якорем родителя
      const replies = (n as { replies?: unknown }).replies
      if (Array.isArray(replies)) {
        const parentHead = text.replace(/^\(в ответ на: [^)]*\)\s*/, "").slice(0, 60)
        for (const rep of replies) {
          const r = rep as { id?: unknown; content?: unknown; createdAt?: unknown; user?: { id?: unknown } }
          if (typeof r.id !== "string") continue
          const repText = commentText(r.content)
          const repUser = r.user?.id
          out.push({
            id: r.id,
            text: `(тред-ответ на «${parentHead}…») ${repText}`,
            createdAt: typeof r.createdAt === "number" ? r.createdAt : created + 1,
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
  parentID?: string
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
      ...(spec.parentID !== undefined ? { parentID: spec.parentID } : {}),
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  return typeof id === "string" && id.startsWith("ses_") ? id : null
}

async function promptSession(sessionID: string, text: string): Promise<boolean> {
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

async function startTask(task: TaskRef, projectName: string, cfg: ProjectCfg, state: FactoryState, attemptsSeed: number): Promise<void> {
  // стоп-сигнал: не спавним новую работу — задача доедет при следующем старте
  if (drain.isStopping()) return
  // ворктри/ветку создаёт сам агент (осмысленные имена — его дело),
  // сессия стартует в основном чекауте репо
  // тайтл: линкованная строка несёт заголовок в доке-теле — деривируем из него
  let title = task.title
  if (title === "" && task.bodyDoc !== "") {
    const doc = await mcp.tool<{ title?: string }>("get_doc", { docId: task.bodyDoc })
    title = (doc.title ?? "").trim()
  }
  if (title === "") title = `задача ${task.rowId.slice(0, 8)}`

  // имя сессии «проект: тайтл» — осмысленно в списке сессий опенкода
  const sessionID = await createSession({
    directory: cfg.local,
    title: `📋 ${projectName}: ${title}`,
  })
  if (sessionID === null) throw new Error("сессия не создалась")

  const sent = await promptSession(
    sessionID,
    spawnPrompt(task.rowId, projectName, cfg.repo),
  )
  if (!sent) throw new Error("spawn-промпт не ушёл")

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
    worktree: "",
    branch: "",
    bodyDoc: task.bodyDoc,
    title,
    lastCommentAt: last?.createdAt ?? 0,
    lastCommentId: last?.id ?? "",
    createdAt: Date.now(),
    attempts: attemptsSeed,
    project: projectName,
    bodyUpdated: await bodyUpdatedAt(task.bodyDoc) ?? undefined,
    agentActive: true, // spawn отправлен — агент работает
    lastStatus: task.status, // спавн из этого статуса (Design) — не рестартить им же
  }
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: сессия ${sessionID}, агент ведёт сам (попыток до этого: ${attemptsSeed})`)
}

/** Чистка state (Done): ворктри/ветку убирает сам агент — демон про git не знает.
 *  Живая ревью-сессия задачи — терминируем (разовая чистка, не watchdog). */
async function finishTask(rowId: string, tracked: TrackedTask, state: FactoryState): Promise<void> {
  log(`задача ${rowId.slice(0, 8)}: Done — чистка state`)
  if (tracked.reviewSessionID !== undefined && tracked.reviewSessionID !== "") {
    await restCall(`/session/${tracked.reviewSessionID}`, { method: "DELETE" }).catch(() => {})
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
    await startTask(task, projectName, cfg, state, seed)
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

/** Ворктри задачи по Branch-ячейке (для result-ревью в контексте ветки). */
async function resolveWorktree(local: string, branch: string): Promise<string> {
  if (branch === "") return ""
  try {
    const out = await sh(["git", "-C", local, "worktree", "list", "--porcelain"])
    let worktree = ""
    for (const line of out.split("\n")) {
      if (line.startsWith("working tree ")) worktree = line.slice("working tree ".length)
      else if (line.startsWith("branch ") && line.endsWith(branch) && worktree !== "") return worktree
    }
  } catch { /* git недоступен — ревьюверу останется cfg.local/PR */ }
  return ""
}

/** Спавн ревьювера: 1 сессия на задачу. Без parentID: наследование каталога
 *  родителем побеждает явный location (проверено на opencode REST) — а ревьюверу
 *  нужен cwd ворктри; от дримера сессию исключает фильтр по agent. Выбрасывает
 *  при фейле. */
async function startReview(
  task: TaskRef, projectName: string, cfg: ProjectCfg, state: FactoryState,
  kind: ReviewKind, tracked: TrackedTask,
): Promise<void> {
  const worktree = kind === "result" ? await resolveWorktree(cfg.local, task.branch) : ""
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
    await notify(tracked,
      `[factory] ревьювер не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH} и напиши комментарий — попробую снова.`)
  }
}

async function tickProject(state: FactoryState, projectName: string, cfg: ProjectCfg): Promise<void> {
  const board = await findBoard(cfg.board)
  const rows = await readTasks(board)

  // 1. новые задачи: Design → девелопер, ревью-статус → ревьювер
  //    (пустой статус = черновик юзера, не трогаем)
  const started = new Set<string>()
  const spawnable = rows.filter((r) => r.status === "Design" || reviewKindOf(r.status) !== null)
  for (const task of untrackedTasks(spawnable, state)) {
    const kind = reviewKindOf(task.status)
    if (kind === null) {
      try {
        await startTask(task, projectName, cfg, state, 0)
        started.add(task.rowId)
      } catch (e) {
        await registerFailure(task, projectName, board, state, 0, e)
      }
      continue
    }
    // ревью-статус без state (даунтайм демона при переходе) — карантинная
    // запись (девелопер неизвестен) + спавн ревьювера
    const comments = (await taskComments(task.bodyDoc)).filter((c) => isUserComment(c, BOT_USER_ID))
    const last = comments.length > 0
      ? comments.reduce((m, c) => (c.createdAt >= m.createdAt ? c : m), comments[0])
      : null
    const entry: TrackedTask = {
      sessionID: "", worktree: "", branch: "",
      bodyDoc: task.bodyDoc, title: task.title, project: projectName,
      lastCommentAt: last?.createdAt ?? 0, lastCommentId: last?.id ?? "",
      createdAt: Date.now(), attempts: 0, reviewAttempts: 0,
    }
    state.tasks[task.rowId] = entry
    try {
      await startReview(task, projectName, cfg, state, kind, entry)
    } catch (e) {
      await failReview(task, board, entry, state, 0, e)
    }
    started.add(task.rowId)
  }

  // 2. активные этой доски (state общий на все проекты)
  for (const rowId of Object.keys(state.tasks)) {
    const tracked = state.tasks[rowId]
    if ((tracked.project || "uwuwu") !== projectName) continue
    if (started.has(rowId)) continue // стартован этим тиком — rows устарели
    const row = rows.find((r) => r.rowId === rowId)
    if (row === undefined) {
      // строка исчезла с доски — чистим за собой
      log(`задача ${rowId.slice(0, 8)}: строка исчезла — чистка`)
      await finishTask(rowId, tracked, state)
      continue
    }

    // Done (мерж-крон агента, Spike-агент, руки юзера) → чистка ворктри
    if (row.status === "Done") {
      log(`задача ${rowId.slice(0, 8)}: Done (закрыта агентом/юзером) — чистка`)
      await finishTask(rowId, tracked, state)
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

    switch (action.t) {
      case "spawn-reviewer": {
        const seed = tracked.reviewAttempts ?? 0
        try {
          await startReview(row, tracked.project || projectName, cfg, state, action.kind, tracked)
          consumeComments()
          await refreshBodyBaseline()
          saveState(state)
        } catch (e) {
          await failReview(row, board, tracked, state, seed, e)
        }
        break
      }
      case "rerun-reviewer": {
        const worktree = action.kind === "result" ? await resolveWorktree(cfg.local, row.branch) : ""
        const ok = await promptSession(
          tracked.reviewSessionID ?? "",
          reviewRerunPrompt(rowId, action.kind, { branch: row.branch, worktree, pr: row.pr }),
        )
        if (ok) {
          consumeComments()
          await refreshBodyBaseline()
          saveState(state)
        } else {
          log(`задача ${rowId.slice(0, 8)}: доревью-промпт не ушёл — следующий тик перепроверит сессию`)
        }
        break
      }
      case "comments-to-reviewer": {
        const ok = await promptSession(tracked.reviewSessionID ?? "", reviewCommentsPrompt(rowId, fresh))
        if (ok) {
          consumeComments()
          saveState(state)
        } else {
          log(`задача ${rowId.slice(0, 8)}: комменты ревьюверу не ушли — граница не сдвинута`)
        }
        break
      }
      case "return-to-dev": {
        const devLiveness = tracked.sessionID === "" ? "dead" : await sessionLiveness(tracked.sessionID)
        if (devLiveness === "alive") {
          const ok = await promptSession(tracked.sessionID, reviewReturnPrompt(rowId))
          if (!ok) log(`задача ${rowId.slice(0, 8)}: возврат из ревью — промпт девелоперу не ушёл`)
        } else {
          // сессия девелопера потеряна — свежий старт (контекст живёт в задаче),
          // ревью-сессию сохраняем: инвариант 1 задача = 1 ревьювер
          const savedReview = tracked.reviewSessionID
          const savedReviewAttempts = tracked.reviewAttempts
          try {
            await startTask(row, tracked.project || projectName, cfg, state, tracked.attempts)
            const reborn = state.tasks[rowId]
            if (reborn !== undefined && savedReview !== undefined) {
              reborn.reviewSessionID = savedReview
              reborn.reviewAttempts = savedReviewAttempts
              saveState(state)
            }
            log(`задача ${rowId.slice(0, 8)}: возврат из ревью при мёртвом девелопере — рестарт сессии`)
          } catch (e) {
            await registerFailure(row, tracked.project || projectName, board, state, tracked.attempts, e)
          }
        }
        break
      }
      case "noop":
        break
    }

    // запоминаем наблюдённый статус — граница перехода для след. тика
    if (tracked.lastStatus !== row.status) {
      tracked.lastStatus = row.status
      saveState(state)
    }

    // ——— пайп девелопера: комменты/тело/живость (комменты мог съесть ревьювер) ———
    const devFresh = commentsConsumed ? [] : fresh

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

    // будильник правок ТЕЛА — немедленно, без отложений; в ревью-статусе
    // адресат — ревьювер (мяч у него), иначе девелопер
    const updated = await bodyUpdatedAt(tracked.bodyDoc)
    const bodyChanged = updated !== null
      && (tracked.bodyUpdated ?? "") !== ""
      && updated > (tracked.bodyUpdated ?? "")
    if (bodyChanged) {
      const target = reviewKindOf(row.status) !== null && (tracked.reviewSessionID ?? "") !== ""
        ? tracked.reviewSessionID ?? ""
        : tracked.sessionID
      if (devFresh.length === 0) {
        log(`задача ${rowId.slice(0, 8)}: тело менялось → wakeup немедленно (${target === tracked.sessionID ? "девелопер" : "ревьювер"})`)
        if (await promptSession(target, bodyEditPrompt(rowId))) {
          tracked.bodyUpdated = updated
          saveState(state)
        }
        continue
      }
      // комменты ниже разбудят той же пачкой — тело агент перечитает сам
      tracked.bodyUpdated = updated
      saveState(state)
    }

    log(`задача ${rowId.slice(0, 8)}: ${devFresh.length} новых коммент(ов) → сессия`)
    if (await promptSession(tracked.sessionID, wakeupPrompt(rowId, devFresh))) {
      const lastComment = devFresh[devFresh.length - 1]
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      tracked.agentActive = true
      saveState(state)
    } else {
      log(`задача ${rowId.slice(0, 8)}: промпт не ушёл — граница комментов не сдвинута`)
    }
  }
}

let tickCount = 0

async function main(): Promise<void> {
  if (!acquireLock({ stateDir: STATE_DIR, onStop: drain.stop })) {
    console.error("другой живой инстанс uwuwu-factory держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  rotateLog()
  await mcp.connect()
  log(`uwuwu-factory: старт (проекты: ${Object.keys(PROJECTS).join(", ")}, poll=${POLL_SEC}s, model=${MODEL.id})`)
  const state = loadState()
  log(`восстановлено задач: ${Object.keys(state.tasks).length}`)
  while (true) {
    for (const [name, cfg] of Object.entries(PROJECTS)) {
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

void main()
