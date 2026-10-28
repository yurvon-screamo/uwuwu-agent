// uwuwu-factory — будильник фабрики разработки на AFFiNE.
// Вся логика пайпа — у агента (глобальный AGENTS.md, раздел «Работа над
// задачей»). Демон делает ровно три вещи:
//   1. Design на доске (пустой статус = черновик юзера, не трогаем) →
//      сессия в чекауте репо + короткий spawn-промпт (ворктри/ветку
//      создаёт сам агент)
//   2. новые комменты/правки тела юзера → wakeup сессии (мёртвая → карантин,
//      коммент юзера → рестарт)
//   3. Done (кто бы ни поставил — мерж-крон агента или юзер) → чистка ворктри
// Запуск: systemd --user (uwuwu-factory.service). Тик: 3 минуты.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "./mcp"
import {
  newUserComments,
  untrackedTasks,
  isUserComment,
  spawnPrompt,
  wakeupPrompt,
  bodyEditPrompt,
  isValidTracked,
  snapshotText,
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
const LOCK_DIR = join(STATE_DIR, "lock")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-factory.log")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
const POLL_SEC = Number(process.env.UWUWU_FACTORY_POLL_SEC ?? 180)
/** разовый тик и выход — для живого смоук-теста */
const ONCE = process.env.UWUWU_FACTORY_ONCE === "1"
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const
const SPAWN_TIMEOUT_MS = 60_000
const REST_TIMEOUT_MS = 15_000
const MAX_ATTEMPTS = 3

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

// ——— одиночный инстанс: lock-каталог + pid, самоосвобождение по SIGTERM ———

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
    } catch { /* pid-файла нет или процесс мёртв — захватываем брошенный lock */ }
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
 *  задачах с длинной историей: 50-окно роняет свежие комменты). */
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
      out.push({ id: n.id, text: snapshotText(n.content), createdAt: created })
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
  const auth = "Basic " + Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")
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

async function createSession(worktree: string, title: string): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: `📋 ${title.replace(/[\r\n]+/g, " ").slice(0, 60)}`,
      model: { providerID: MODEL.providerID, id: MODEL.id },
      location: { directory: worktree },
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

/** Последний тип сообщения сессии: "idle" = агент закончил и ждёт. */
async function lastMessageType(sessionID: string): Promise<string | null> {
  try {
    const msgs = await restCall(`/session/${sessionID}/message`) as { data?: unknown[] } | unknown[]
    const arr = (msgs as { data?: unknown[] }).data ?? (msgs as unknown[])
    if (!Array.isArray(arr) || arr.length === 0) return null
    return (arr[arr.length - 1] as { type?: unknown }).type as string | null
  } catch {
    return null
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
  const sessionID = await createSession(cfg.local, `${projectName}: ${title}`)
  if (sessionID === null) throw new Error("сессия не создалась")

  const sent = await promptSession(
    sessionID,
    spawnPrompt(task.rowId, projectName, cfg.repo),
  )
  if (!sent) throw new Error("spawn-промпт не ушёл")

  // граница комментов ДО старта — не будить агента старыми;
  // маркер = последний юзерский коммент (позиционный поиск в
  // newUserComments идёт по юзерскому списку)
  const comments = (await taskComments(task.bodyDoc)).filter(isUserComment)
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
  }
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: сессия ${sessionID}, агент ведёт сам (попыток до этого: ${attemptsSeed})`)
}

/** Чистка state (Done): ворктри/ветку убирает сам агент — демон про git не знает. */
async function finishTask(rowId: string, tracked: TrackedTask, state: FactoryState): Promise<void> {
  log(`задача ${rowId.slice(0, 8)}: Done — чистка state`)
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
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState, seed: number, err: unknown,
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
      `🤖 [factory] не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH} и напиши комментарий — попробую снова.`)
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
  saveState(state)
  try {
    await startTask(task, projectName, cfg, state, seed)
  } catch (e) {
    await registerFailure(task, projectName, cfg, board, state, seed, e)
  }
}

async function tickProject(state: FactoryState, projectName: string, cfg: ProjectCfg): Promise<void> {
  const board = await findBoard(cfg.board)
  const rows = await readTasks(board)

  // 1. новые Design-задачи (пустой статус = черновик юзера, не трогаем)
  const started = new Set<string>()
  for (const task of untrackedTasks(rows.filter((r) => r.status === "Design"), state)) {
    try {
      await startTask(task, projectName, cfg, state, 0)
      started.add(task.rowId)
    } catch (e) {
      await registerFailure(task, projectName, cfg, board, state, 0, e)
    }
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
    // юзер закрыл руками через UI/фильтр — доводим статус до конца? нет:
    // Done ставим только мы (по мержу), но исчезновение In Progress с доски =
    // удаление строки — уже обработано выше.

    // Done (мерж-крон агента, Spike-агент, руки юзера) → чистка ворктри
    if (row.status === "Done") {
      log(`задача ${rowId.slice(0, 8)}: Done (закрыта агентом/юзером) — чистка`)
      await finishTask(rowId, tracked, state)
      continue
    }

    // юзер вернул живую задачу в Design = «перепланируй/переделай»:
    // молчать нельзя (сигнал потерян), затирать без предупреждения тоже
    if (row.status === "Design" && tracked.sessionID !== "") {
      log(`задача ${rowId.slice(0, 8)}: юзер вернул в Design — перезапуск`)
      await notify(tracked,
        "🤖 [factory] вижу снос статуса — перезапускаю задачу. Несмерженное в ворктри будет перезаписано с master.")
      tracked.sessionID = ""
      saveState(state)
      const comments = (await taskComments(tracked.bodyDoc)).filter(isUserComment)
      const last = comments.length > 0 ? comments[comments.length - 1] : null
      if (last !== null) {
        tracked.lastCommentAt = last.createdAt
        tracked.lastCommentId = last.id
        saveState(state)
      }
      try {
        await startTask(row, tracked.project || projectName, cfg, state, tracked.attempts)
      } catch (e) {
        await registerFailure(row, tracked.project || projectName, cfg, board, state, tracked.attempts, e)
      }
      continue
    }

    const comments = await taskComments(tracked.bodyDoc)
    const fresh = newUserComments(comments, { at: tracked.lastCommentAt, id: tracked.lastCommentId })

    if (tracked.sessionID === "") {
      // карантин: ждём явной активности юзера
      if (fresh.length === 0) continue
      await restartTask(row, tracked.project || projectName, cfg, board, state, fresh)
      continue
    }

    if (fresh.length === 0) {
      // юзер молчит, но сессия могла умереть посреди прогона
      const quiet = await sessionLiveness(tracked.sessionID)
      if (quiet === "dead") {
        log(`задача ${rowId.slice(0, 8)}: сессия умерла (юзер молчит) — карантин + уведомление`)
        tracked.sessionID = ""
        saveState(state)
        await notify(tracked,
          "🤖 [factory] сессия разработки потеряна. Напиши комментарий — перезапущу (несохранённое в ворктри будет перезаписано с master).")
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
      await restartTask(row, tracked.project || projectName, cfg, board, state, fresh)
      continue
    }

    // будильник правок ТЕЛА: активного агента не дёргаем; после его хода
    // (idle) любое изменение с базлайна — wakeup, атрибуция на агенте
    // (он знает, что писал сам: чужое учтёт, своё пропустит молча)
    const idle = (await lastMessageType(tracked.sessionID)) === "idle"
    const updated = await bodyUpdatedAt(tracked.bodyDoc)
    const bodyChanged = updated !== null
      && (tracked.bodyUpdated ?? "") !== ""
      && updated > (tracked.bodyUpdated ?? "")
    if (idle && bodyChanged) {
      log(`задача ${rowId.slice(0, 8)}: тело менялось → wakeup (атрибуция на агенте)`)
      if (await promptSession(tracked.sessionID, bodyEditPrompt(rowId))) {
        tracked.bodyUpdated = updated
        tracked.agentActive = true
        saveState(state)
      }
      continue
    }
    if (idle && tracked.agentActive === true && updated !== null) {
      // ход агента завершён без правок тела — просто снять флаг
      tracked.agentActive = false
      tracked.bodyUpdated = updated
      saveState(state)
    }

    log(`задача ${rowId.slice(0, 8)}: ${fresh.length} новых коммент(ов) → сессия`)
    if (await promptSession(tracked.sessionID, wakeupPrompt(rowId, fresh))) {
      const last = fresh[fresh.length - 1]
      tracked.lastCommentAt = last.createdAt
      tracked.lastCommentId = last.id
      tracked.agentActive = true
      saveState(state)
    } else {
      log(`задача ${rowId.slice(0, 8)}: промпт не ушёл — граница комментов не сдвинута`)
    }
  }
}

let tickCount = 0

async function main(): Promise<void> {
  if (!acquireLock()) {
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
    await new Promise((r) => setTimeout(r, POLL_SEC * 1000))
  }
}

void main()
