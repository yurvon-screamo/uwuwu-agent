// uwuwu-factory — демон фабрики разработки на AFFiNE.
// Поллит канбан-доску (Status=Todo), на каждую задачу поднимает сессию
// девелопера в своём ворктри от свежего master, мостит комменты юзера
// в сессию (фильтр 🤖), следит за PR (Review → MERGED → Done + чистка).
// Запуск: systemd --user (uwuwu-factory.service). Тик: 3 минуты.

import { appendFileSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import * as mcp from "./mcp"
import {
  newUserComments,
  untrackedTasks,
  isUserComment,
  kickoffPrompt,
  wakeupPrompt,
  worktreePaths,
  isValidTracked,
  snapshotText,
  type FactoryState,
  type TaskComment,
  type TaskRef,
  type TrackedTask,
} from "./factory-lib"

// ——— конфиг проекта (v1: uwuwu; расширяется картой при раскатке) ———
const PROJECT = "uwuwu"
const BOARD_TITLE = "uwuwu Tasks"
const REPO = "yurvon-screamo/uwuwu-agent"
const LOCAL = join(homedir(), "uwuwu-agent")
const WORKTREES_ROOT = join(homedir(), ".local/share/uwuwu-factory/worktrees")
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
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 2))
  renameSync(tmp, STATE_FILE)
}

// ——— процессы (git/gh/uwuwu-cli) с таймаутом ———

async function sh(cmd: string[], opts: { cwd?: string } = {}): Promise<string> {
  const proc = Bun.spawn({ cmd, cwd: opts.cwd ?? LOCAL, stdout: "pipe", stderr: "pipe" })
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

async function shOk(cmd: string[], opts: { cwd?: string } = {}): Promise<boolean> {
  try {
    await sh(cmd, opts)
    return true
  } catch {
    return false
  }
}

// ——— доска AFFiNE ———

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

async function taskBody(bodyDoc: string): Promise<string> {
  if (bodyDoc === "") return "(тела нет)"
  const r = await mcp.tool<{ markdown?: string }>("export_doc_markdown", { docId: bodyDoc })
  return r.markdown ?? "(пусто)"
}

async function taskComments(bodyDoc: string): Promise<TaskComment[]> {
  if (bodyDoc === "") return []
  const r = await mcp.tool<{
    edges?: {
      node?: {
        id?: string
        content?: unknown
        resolved?: boolean
      }
    }[]
  }>("list_comments", { docId: bodyDoc, first: 50 })
  const out: TaskComment[] = []
  for (const e of r.edges ?? []) {
    const n = e.node
    if (n === undefined || typeof n.id !== "string") continue
    const snap = (n.content as { snapshot?: { meta?: { createDate?: unknown } } } | undefined)?.snapshot
    const created = typeof snap?.meta?.createDate === "number" ? snap.meta.createDate : 0
    out.push({ id: n.id, text: snapshotText(n.content), createdAt: created })
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

// ——— жизненный цикл задачи ———

function readdirSafe(p: string): string[] {
  try {
    return readdirSync(p)
  } catch {
    return []
  }
}

/** Сверка на старте: осиротевшие ворктри без state — вычистить. */
function reconcileOrphans(state: FactoryState): void {
  const known = new Set(Object.values(state.tasks).map((t) => t.worktree))
  if (!existsSync(WORKTREES_ROOT)) return
  for (const name of readdirSafe(WORKTREES_ROOT)) {
    const full = join(WORKTREES_ROOT, name)
    if (!known.has(full) && /^task-[a-zA-Z0-9_-]+$/.test(name)) {
      try {
        log(`reconcile: осиротевший ворктри ${full} — удаляю`)
        rmSync(full, { recursive: true, force: true })
        void shOk(["git", "-C", LOCAL, "worktree", "prune"])
        void shOk(["git", "-C", LOCAL, "branch", "-D", `work/${name}`])
      } catch (e) {
        log(`reconcile: ${full} не удался — ${e}`)
      }
    }
  }
}

async function startTask(task: TaskRef, board: BoardIds, state: FactoryState, attemptsSeed: number): Promise<void> {
  const { branch, worktree } = worktreePaths(WORKTREES_ROOT, task.rowId)
  mkdirSync(WORKTREES_ROOT, { recursive: true })

  if (existsSync(worktree)) {
    log(`задача ${task.rowId.slice(0, 8)}: ворктри уже есть — чищу перед пересозданием`)
    rmSync(worktree, { recursive: true, force: true })
  }
  await shOk(["git", "-C", LOCAL, "worktree", "prune"])
  await shOk(["git", "-C", LOCAL, "branch", "-D", branch])

  await sh(["git", "-C", LOCAL, "fetch", "origin", "master"])
  await sh(["git", "-C", LOCAL, "worktree", "add", "-b", branch, worktree, "origin/master"])

  const sessionID = await createSession(worktree, task.title)
  if (sessionID === null) throw new Error("сессия не создалась")

  let body = await taskBody(task.bodyDoc)
  // линкованные строки несут заголовок в теле-доке, не в ячейке:
  // деривируем из первой строки экспорта (обычно «# Заголовок»)
  let title = task.title
  if (title === "" && task.bodyDoc !== "") {
    title = body.split("\n")[0]?.replace(/^#+\s*/, "").trim() ?? ""
    if (title === "") title = `задача ${task.rowId.slice(0, 8)}`
  }
  const sent = await promptSession(sessionID, kickoffPrompt({ ...task, title }, body, REPO))
  if (!sent) throw new Error("kickoff-промпт не ушёл")

  // граница комментов ДО старта — не будить агента старыми;
  // маркер = последний юзерский коммент (позиционный поиск в
  // newUserComments идёт по юзерскому списку)
  const comments = (await taskComments(task.bodyDoc)).filter(isUserComment)
  const last = comments.length > 0
    ? comments.reduce((m, c) => (c.createdAt >= m.createdAt ? c : m), comments[0])
    : null

  state.tasks[task.rowId] = {
    sessionID,
    worktree,
    branch,
    bodyDoc: task.bodyDoc,
    title,
    lastCommentAt: last?.createdAt ?? 0,
    lastCommentId: last?.id ?? "",
    createdAt: Date.now(),
    attempts: attemptsSeed,
  }
  saveState(state)
  await setCells(board, task.rowId, { Status: "In Progress", Branch: branch })
  log(`задача ${task.rowId.slice(0, 8)}: сессия ${sessionID}, In Progress (попыток до этого: ${attemptsSeed})`)
}

/** Полная чистка (Done); state-запись умирает только при подтверждении ФС. */
async function finishTask(rowId: string, tracked: TrackedTask, state: FactoryState): Promise<void> {
  log(`задача ${rowId.slice(0, 8)}: Done — чистка (сессия сохранена для истории)`)
  let clean = true
  if (existsSync(tracked.worktree)) {
    const removed = await shOk(["git", "-C", LOCAL, "worktree", "remove", "--force", tracked.worktree])
    if (!removed) {
      rmSync(tracked.worktree, { recursive: true, force: true })
      await shOk(["git", "-C", LOCAL, "worktree", "prune"])
    }
    if (existsSync(tracked.worktree)) {
      clean = false
      log(`задача ${rowId.slice(0, 8)}: ворктри не удалился — ретрай на след. тике`)
    }
  }
  await shOk(["git", "-C", LOCAL, "branch", "-D", tracked.branch])
  if (clean) {
    delete state.tasks[rowId]
    saveState(state)
  }
}

async function notify(task: TrackedTask, body: string): Promise<void> {
  // один ретрай: потеря коммента недопустима
  if (await commentTask(task.bodyDoc, body)) return
  await new Promise((r) => setTimeout(r, 15_000))
  if (!(await commentTask(task.bodyDoc, body))) log(`уведомление не доставлено: ${body.slice(0, 80)}`)
}

async function registerFailure(
  task: TaskRef, board: BoardIds, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  const entry: TrackedTask = state.tasks[task.rowId] ?? {
    sessionID: "", worktree: "", branch: "",
    bodyDoc: task.bodyDoc, title: task.title,
    lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
  }
  entry.attempts = attempts
  state.tasks[task.rowId] = entry
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    await setCells(board, task.rowId, { Status: "Blocked" })
    await notify(entry,
      `🤖 [factory] не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH} и напиши комментарий — попробую снова.`)
  }
}

/** PR-надзор: Review + PR-url → merged → Done + чистка. */
async function checkMerged(task: TaskRef, board: BoardIds, state: FactoryState): Promise<boolean> {
  const tracked = state.tasks[task.rowId]
  if (tracked === undefined) return false
  let out = ""
  try {
    out = await sh(["gh", "pr", "view", task.pr, "--json", "state"])
  } catch (e) {
    log(`pr view ${task.pr}: ${e}`)
    return false
  }
  try {
    const st = (JSON.parse(out) as { state?: string }).state?.toLowerCase()
    if (st === "merged") {
      await setCells(board, task.rowId, { Status: "Done" })
      await notify(tracked, "🤖 [factory] PR смержен — задача закрыта, ворктри убран.")
      await finishTask(task.rowId, tracked, state)
      return true
    }
  } catch { /* ответ не JSON — на след. тике */ }
  return false
}

async function restartTask(
  task: TaskRef, board: BoardIds, state: FactoryState, fresh: TaskComment[],
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
    await startTask(task, board, state, seed)
  } catch (e) {
    await registerFailure(task, board, state, seed, e)
  }
}

async function tick(state: FactoryState): Promise<void> {
  const board = await findBoard()
  const rows = await readTasks(board)

  // 1. новые Todo
  for (const task of untrackedTasks(rows.filter((r) => r.status === "Todo"), state)) {
    try {
      await startTask(task, board, state, 0)
    } catch (e) {
      await registerFailure(task, board, state, 0, e)
    }
  }

  // 2. активные (в state)
  for (const rowId of Object.keys(state.tasks)) {
    const tracked = state.tasks[rowId]
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

    if (row.status === "Review" && row.pr !== "") {
      if (await checkMerged(row, board, state)) continue
    }

    const comments = await taskComments(tracked.bodyDoc)
    const fresh = newUserComments(comments, { at: tracked.lastCommentAt, id: tracked.lastCommentId })

    if (tracked.sessionID === "") {
      // карантин: ждём явной активности юзера
      if (fresh.length === 0) continue
      await restartTask(row, board, state, fresh)
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
      await restartTask(row, board, state, fresh)
      continue
    }
    log(`задача ${rowId.slice(0, 8)}: ${fresh.length} новых коммент(ов) → сессия`)
    if (await promptSession(tracked.sessionID, wakeupPrompt(rowId, fresh))) {
      const last = fresh[fresh.length - 1]
      tracked.lastCommentAt = last.createdAt
      tracked.lastCommentId = last.id
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
  log(`uwuwu-factory: старт (board=${BOARD_TITLE}, repo=${REPO}, poll=${POLL_SEC}s, model=${MODEL.id})`)
  const state = loadState()
  log(`восстановлено задач: ${Object.keys(state.tasks).length}`)
  reconcileOrphans(state)
  while (true) {
    try {
      await tick(state)
    } catch (e) {
      log(`tick: ${e}`)
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
