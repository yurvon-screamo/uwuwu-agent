// task-run-lib.ts — задачный контур uwuwu-dreamer: единица анализа = задача
// (все её сессии пакетом), триггер = закрытие задачи. Сигналы фабрики —
// файлы очереди (start — мерж в активный набор, done — запуск прогона);
// сбор сессий — BFS по parent_id от полной истории корней; никаких DELETE.
// Чистые функции: демон (dreamer.ts) несёт REST/FS-обвязку.

import type { Database } from "bun:sqlite"
import { fetchSessionsByParents, fetchSessionById, type SessionRow } from "./session-row"

// ——— типы ———

/** Сигнал спавна сессии задачи (фабрика пишет при каждом старте
 *  дев/ревью/тест-сессии, включая рестарты — история корней не теряется). */
export interface TaskStartSignal {
  rowId: string
  project: string
  title: string
  sessionID: string
  /** каталог env-задачи; "" — env:false (сессия живёт в directory) */
  envDir: string
  /** мастер-чекаут проекта — cwd дример-сессии задачного прогона */
  directory: string
  ts: number
}

/** Сигнал закрытия задачи: запуск задачного прогона. rootSessions — полная
 *  история корней (unique(sessionHistory) фабрики) — BFS накрывает все
 *  поколения сессий, включая осиротевшие при рестартах. */
export interface TaskDoneSignal {
  rowId: string
  project: string
  title: string
  rootSessions: string[]
  branch: string
  pr: string
  envDir: string
  directory: string
  closedAt: number
}

export type QueuedSignal =
  | { kind: "start"; signal: TaskStartSignal }
  | { kind: "done"; signal: TaskDoneSignal }

export type TaskRunStatus = "pending" | "running" | "done" | "failed" | "gave_up"

/** Трек задачного прогона одной задачи (state.json: taskRuns[rowId]). */
export interface TrackedTaskRun {
  status: TaskRunStatus
  project: string
  title: string
  rootSessions: string[]
  branch: string
  pr: string
  envDir: string
  /** мастер-чекаут проекта (cwd дример-сессии) */
  directory: string
  /** ms-время закрытия задачи (сигнала done) */
  closedAt: number
  /** max timeUpdated сессий на момент спавна — граница дедупа переоткрытий;
   *  0 = ещё не анализировалась */
  lastProcessedUpdate: number
  attempts: number
  /** сессия дримера, обслуживающая текущий/последний прогон */
  dreamerSes: string | null
  /** момент последнего wakeup-промпта; 0 = не было */
  wokenAt: number
  at: number
}

/** Активный набор: sessionID → rowId живой задачи. Наполняется start-сигналами,
 *  посессионный контур скипает эти сессии (единый критерий принадлежности для
 *  всех проектов — без directory-эвристик). */
export type ActiveSessions = Record<string, string>

export function mergeActiveSession(active: ActiveSessions, sid: string, rowId: string): void {
  active[sid] = rowId
}

/** Снять все сессии задачи (финал прогона: done — сессии обработаны пакетом,
 *  gave_up — fallback посессионной пост-обработки). */
export function dropTaskActiveSessions(active: ActiveSessions, rowId: string): void {
  for (const [sid, owner] of Object.entries(active)) {
    if (owner === rowId) delete active[sid]
  }
}

// ——— normalize / validate (миграция state.json между версиями) ———

const RUN_STATUSES: TaskRunStatus[] = ["pending", "running", "done", "failed", "gave_up"]

const isSesList = (v: unknown): v is string[] =>
  Array.isArray(v) && v.every((s) => typeof s === "string" && s.startsWith("ses_"))

/** Старый state без новых полей нормализуется; битая запись — null (пропуск). */
export function normalizeTrackedRun(v: unknown): TrackedTaskRun | null {
  if (v === null || typeof v !== "object") return null
  const t = v as Record<string, unknown>
  if (typeof t.status !== "string" || !RUN_STATUSES.includes(t.status as TaskRunStatus)) return null
  if (typeof t.project !== "string" || typeof t.title !== "string") return null
  if (!isSesList(t.rootSessions)) return null
  if (typeof t.branch !== "string" || typeof t.pr !== "string" || typeof t.envDir !== "string") return null
  if (typeof t.directory !== "string" || typeof t.closedAt !== "number") return null
  if (typeof t.lastProcessedUpdate !== "number" || typeof t.attempts !== "number" || typeof t.at !== "number") return null
  if (t.dreamerSes !== null && typeof t.dreamerSes !== "string") return null
  return {
    status: t.status as TaskRunStatus,
    project: t.project,
    title: t.title,
    rootSessions: t.rootSessions,
    branch: t.branch,
    pr: t.pr,
    envDir: t.envDir,
    directory: t.directory,
    closedAt: t.closedAt,
    lastProcessedUpdate: t.lastProcessedUpdate,
    attempts: t.attempts,
    dreamerSes: t.dreamerSes as string | null,
    wokenAt: typeof t.wokenAt === "number" ? t.wokenAt : 0,
    at: t.at,
  }
}

/** Активный набор из state: sid→rowId, оба — строки. */
export function normalizeActiveSessions(v: unknown): ActiveSessions {
  if (v === null || typeof v !== "object") return {}
  const out: ActiveSessions = {}
  for (const [sid, rowId] of Object.entries(v as Record<string, unknown>)) {
    if (sid.startsWith("ses_") && typeof rowId === "string") out[sid] = rowId
  }
  return out
}

// ——— парсинг очереди ———

const isRowId = (v: unknown): v is string => typeof v === "string" && /^[0-9]{14}-[a-z0-9]+$/.test(v)

function parseStartSignal(raw: Record<string, unknown>): TaskStartSignal | null {
  if (!isRowId(raw.rowId)) return null
  if (typeof raw.project !== "string" || typeof raw.title !== "string") return null
  if (typeof raw.sessionID !== "string" || !raw.sessionID.startsWith("ses_")) return null
  if (typeof raw.envDir !== "string" || typeof raw.directory !== "string") return null
  if (typeof raw.ts !== "number") return null
  return {
    rowId: raw.rowId,
    project: raw.project,
    title: raw.title,
    sessionID: raw.sessionID,
    envDir: raw.envDir,
    directory: raw.directory,
    ts: raw.ts,
  }
}

function parseDoneSignal(raw: Record<string, unknown>): TaskDoneSignal | null {
  if (!isRowId(raw.rowId)) return null
  if (typeof raw.project !== "string" || typeof raw.title !== "string") return null
  if (!isSesList(raw.rootSessions)) return null
  if (typeof raw.branch !== "string" || typeof raw.pr !== "string") return null
  if (typeof raw.envDir !== "string" || typeof raw.directory !== "string") return null
  if (typeof raw.closedAt !== "number") return null
  return {
    rowId: raw.rowId,
    project: raw.project,
    title: raw.title,
    rootSessions: raw.rootSessions,
    branch: raw.branch,
    pr: raw.pr,
    envDir: raw.envDir,
    directory: raw.directory,
    closedAt: raw.closedAt,
  }
}

/** Парсинг файла очереди по имени: `start-<rowId>.json` / `done-<rowId>.json`.
 *  null — битый файл или чужое имя (демон логирует и удаляет битые). */
export function parseQueuedSignal(fileName: string, content: string): QueuedSignal | null {
  if (fileName.startsWith("start-") && fileName.endsWith(".json")) {
    try {
      const signal = parseStartSignal(JSON.parse(content) as Record<string, unknown>)
      return signal === null ? null : { kind: "start", signal }
    } catch {
      return null
    }
  }
  if (fileName.startsWith("done-") && fileName.endsWith(".json")) {
    try {
      const signal = parseDoneSignal(JSON.parse(content) as Record<string, unknown>)
      return signal === null ? null : { kind: "done", signal }
    } catch {
      return null
    }
  }
  return null
}

// ——— сбор сессий задачи ———

/** BFS по parent_id от полной истории корней. Исключения — общие для всех
 *  выборок (dreamer, e2e-синтетика). Лимит — гвард от взрыва графа. */
export function collectTaskSessions(
  db: Database,
  rootSessions: string[],
  maxSessions = 500,
): SessionRow[] {
  const seen = new Set<string>()
  const out: SessionRow[] = []
  let frontier: string[] = []
  for (const sid of rootSessions) {
    if (!sid.startsWith("ses_") || seen.has(sid)) continue
    seen.add(sid)
    const row = fetchSessionById(db, sid)
    if (row !== null) out.push(row)
    frontier.push(sid)
  }
  while (frontier.length > 0 && out.length < maxSessions) {
    const children = fetchSessionsByParents(db, frontier)
    frontier = []
    for (const child of children) {
      if (seen.has(child.id)) continue
      seen.add(child.id)
      if (out.length >= maxSessions) break
      out.push(child)
      frontier.push(child.id)
    }
  }
  return out
}

/** Max timeUpdated батча — граница дедупа переоткрытий (lastProcessedUpdate). */
export function batchMaxUpdated(sessions: SessionRow[]): number {
  return sessions.reduce((max, s) => Math.max(max, s.timeUpdated), 0)
}

// ——— state-машина задачного прогона ———

export interface TaskRunOutcome {
  /** сессия дримера отвечала (assistant-сообщение) — работа сделана */
  replied: boolean
  /** сессия дримера исчезла из opencode (404/400) */
  dead: boolean
  /** сессия жива, но молчит дольше таймаута прогона */
  timedOut: boolean
}

/** Переход state-машины по исходу проверки running-прогона. done фиксирует
 *  границу дедупа (max timeUpdated батча); паритет посессионного applyRunOutcome. */
export function applyTaskRunOutcome(
  tracked: TrackedTaskRun,
  batchMaxUpdated: number,
  outcome: TaskRunOutcome,
  maxAttempts: number,
): "done" | "failed" | "gave_up" | "keep" {
  if (!outcome.dead && !outcome.timedOut) return "keep"
  if (outcome.replied) {
    tracked.status = "done"
    tracked.lastProcessedUpdate = Math.max(tracked.lastProcessedUpdate, batchMaxUpdated)
    tracked.at = Date.now()
    return "done"
  }
  tracked.attempts += 1
  tracked.status = tracked.attempts >= maxAttempts ? "gave_up" : "failed"
  tracked.at = Date.now()
  return tracked.status
}

/** Забор done-сигнала: pending-трек. Перезапись при done/gave_up — новый цикл
 *  (переоткрытие): граница дедупа прошлого прогона сохраняется, попытки с нуля. */
export function acceptDoneSignal(
  prev: TrackedTaskRun | undefined,
  signal: TaskDoneSignal,
  now: number,
): TrackedTaskRun {
  return {
    status: "pending",
    project: signal.project,
    title: signal.title,
    rootSessions: signal.rootSessions,
    branch: signal.branch,
    pr: signal.pr,
    envDir: signal.envDir,
    directory: signal.directory,
    closedAt: signal.closedAt,
    lastProcessedUpdate: prev?.lastProcessedUpdate ?? 0,
    attempts: 0,
    dreamerSes: null,
    wokenAt: 0,
    at: now,
  }
}

// ——— промпт ———

export interface TaskPromptInput {
  rowId: string
  project: string
  title: string
  branch: string
  pr: string
  /** мастер-чекаут проекта (git-истина после мерджа) */
  directory: string
  /** граница дедупа переоткрытий; 0 — первый прогон */
  lastProcessedUpdate: number
  sessions: SessionRow[]
  auditPath: string
  wikiOpsPath: string
  dbPath: string
  /** повторная попытка (прошлый прогон прервался) — та же сессия дримера */
  retry?: boolean
}

/**
 * Промпт задачного прогона — только входные данные; вся методика живёт в
 * agents/dreamer.md (режим «задача»).
 */
export function taskRunPrompt(input: TaskPromptInput): string {
  const head = input.retry === true
    ? `Дример: повторная попытка — прошлый прогон задачи прервался. Продолжи анализ с того места.`
    : `Дример: проанализируй закрытую задачу ${input.rowId} («${input.title}», проект ${input.project}).`
  const lead = [
    head,
    `Единица анализа — задача: сессии ниже — её полный жизненный цикл (дев, ревью,`,
    `тестирование, дочерние); тело задачи — каркас выноса опыта (постановка, круги`,
    `ревью/теста, финальный отчёт инженера).`,
    ``,
    `Входы:`,
    `- тело задачи: uwuwu-cli task get ${input.rowId} --project ${input.project}`,
    `- дампы сессий (по одной на сессию):`,
  ]
  const sessionLines = input.sessions.map((s) => {
    const label = `${s.id} (${s.title ?? "без тайтла"}, ${s.agent ?? "agent"}, ${new Date(s.timeUpdated).toISOString()})`
    return `  - ${label}: python3 "${input.auditPath}" --db "${input.dbPath}" --compact session ${s.id}`
  })
  const tail = [
    ``,
    `- БД опенкода (readonly, прямые SELECT при необходимости): ${input.dbPath}`,
    `- git-контекст: каталог ${input.directory} (master после мерджа; ветка ${input.branch || "—"}),`,
    `  невлитая работа — gh pr view ${input.pr || "<PR отсутствует>"}`,
    `- чтение вики: uwuwu-cli read "<slug>"; поиск: uwuwu-cli search`,
    `- запись в вики (абсолютный путь — cwd у тебя чужой проект): "${input.wikiOpsPath}"`,
  ]
  const dedup = input.lastProcessedUpdate > 0
    ? [
      ``,
      `Повторный прогон (задача переоткрывалась): сессии с сообщениями до`,
      `${new Date(input.lastProcessedUpdate).toISOString()} (мс: ${input.lastProcessedUpdate}) уже`,
      `анализировались прошлым прогоном — дельта после границы; дедуп по своим`,
      `прошлым записям этой задачи обязателен.`,
    ]
    : []
  const finish = [
    ``,
    `Действуй строго по своей методике (режим «задача»). Заверши одной-двумя`,
    `строками: что записал (тайтлы) / «нечего взять» / причина недоступности вики.`,
  ]
  return [...lead, ...sessionLines, ...tail, ...dedup, ...finish].join("\n")
}
