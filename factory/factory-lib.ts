// factory-lib.ts — чистые функции uwuwu-factory: состояние, комменты, промпты.
// Задача = строка канбана AFFiNE (rowBlockId) + док-тело (bodyDocId).
// Комментарии афины не различают авторов — фильтр по маркеру 🤖/[factory].

export interface TaskRef {
  rowId: string
  bodyDoc: string
  title: string
  status: string
  pr: string
  branch: string
}

export interface TaskComment {
  id: string
  text: string
  /** ms-таймстамп из снапшота (надёжный порядок) */
  createdAt: number
}

/** Состояние одной задачи фабрики. */
export interface TrackedTask {
  sessionID: string
  worktree: string
  branch: string
  bodyDoc: string
  title: string
  /** ISO updatedAt тела при последнем поглощённом состоянии */
  bodyUpdated?: string
  /** агент в работе (не idle): его правки тела не будят его же */
  agentActive?: boolean
  /** проект (ключ конфига демона) */
  project: string
  /** ms-время последнего обработанного коммента */
  lastCommentAt: number
  /** id последнего обработанного коммента (тай-брейк в одну мс) */
  lastCommentId: string
  createdAt: number
  attempts: number
}

export interface FactoryState {
  tasks: Record<string, TrackedTask>
}

/** Комменты агента — с этим префиксом, чтобы не петлить себя. */
export const AGENT_MARK = "🤖"

/** Юзерский коммент: не наш (🤖), не служебный ([factory]). */
export function isUserComment(c: TaskComment): boolean {
  const body = c.text.trimStart()
  if (body.startsWith(AGENT_MARK)) return false
  if (body.startsWith("[factory]")) return false
  return body.trim() !== ""
}

/** Новые юзерские комменты после lastSeen, по порядку создания. */
export function newUserComments(
  comments: TaskComment[],
  lastSeen: { at: number; id: string },
): TaskComment[] {
  const users = comments
    .filter(isUserComment)
    .sort((a, b) => (a.createdAt === b.createdAt ? a.id.localeCompare(b.id) : a.createdAt - b.createdAt))
  if (lastSeen.id === "" && lastSeen.at === 0) return users
  const idx = users.findIndex((c) => c.id === lastSeen.id)
  if (idx >= 0) return users.slice(idx + 1)
  return users.filter((c) => c.createdAt > lastSeen.at)
}

/** Строки Todo, которых ещё нет в состоянии. */
export function untrackedTasks(rows: TaskRef[], state: FactoryState): TaskRef[] {
  return rows.filter((r) => state.tasks[r.rowId] === undefined)
}

/** Конкатенация текстовых дельт дерева блоков коммента/дока. */
export function snapshotText(content: unknown): string {
  const out: string[] = []
  const walk = (node: unknown): void => {
    if (node === null || typeof node !== "object") return
    const n = node as Record<string, unknown>
    const props = n.props as Record<string, unknown> | undefined
    const text = props?.text as Record<string, unknown> | undefined
    const delta = text?.delta as { insert?: unknown }[] | undefined
    if (Array.isArray(delta)) {
      for (const d of delta) if (typeof d.insert === "string") out.push(d.insert)
    }
    const children = n.children as unknown[] | undefined
    if (Array.isArray(children)) for (const ch of children) walk(ch)
  }
  const snap = (content as Record<string, unknown> | null)?.snapshot as Record<string, unknown> | undefined
  walk(snap?.blocks)
  return out.join("").trim()
}

/** Стартовый промпт будильника: пайп живёт в AGENTS.md (глобально),
 *  демон знает только «какая задача» и «где работать». */
export function spawnPrompt(rowId: string, project: string, repo: string): string {
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}).`,
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей»: uwuwu-cli task get — тело, тип, комменты.",
    "Репо — текущий каталог: сам создай worktree+ветку от origin/master с осмысленным именем",
    "(по сути задачи, не id), 🤖-коммент «взялся» и статусы двигай task edit'ом. Вопросы/отчёты — 🤖.",
  ].join("\n")
}

/** Будильник: юзер отредактировал ТЕЛО задачи (не коммент). */
export function bodyEditPrompt(rowId: string): string {
  return [
    `[factory] Юзер отредактировал ТЕЛО задачи ${rowId} (не коммент).`,
    "",
    "Перечитай тело (uwuwu-cli task get — печатает тело целиком), найди что изменилось,",
    "учти изменения в работе. Ответь/доложи по пайпу — комментом 🤖.",
  ].join("\n")
}

/** Промпт-будильник на новые комменты юзера. */
export function wakeupPrompt(rowId: string, comments: TaskComment[]): string {
  const quoted = comments.map((c) => c.text.trim()).join("\n---\n")
  return [
    `[factory] В задаче ${rowId} новые комментарии от пользователя:`,
    "",
    quoted,
    "",
    "Отреагируй по пайпу: ответь/исправь. Ответ пользователю — uwuwu-cli task comment (🤖), не текст в сессии.",
  ].join("\n")
}

/** Ветка и путь ворктри для задачи (row-id может начинаться с дефиса). */
export function worktreePaths(root: string, rowId: string): { branch: string; worktree: string } {
  const safe = rowId.replace(/^-+/, "").replace(/[^a-zA-Z0-9_-]/g, "").slice(0, 12) || "task"
  return {
    branch: `work/task-${safe}`,
    worktree: `${root}/task-${safe}`,
  }
}

/** Валидация записи state.json. Три легальных формата: полный
 *  (ses_ + пути), чистый карантин (всё пусто) и гибрид (сессия умерла,
 *  пути живы — работа сохраняется до решения юзера). worktree/branch —
 *  legacy (ворктри теперь делает агент, демону не нужен). */
export function isValidTracked(v: unknown): v is TrackedTask {
  if (v === null || typeof v !== "object") return false
  const t = v as Record<string, unknown>
  if (
    typeof t.lastCommentAt !== "number" ||
    typeof t.lastCommentId !== "string" ||
    typeof t.createdAt !== "number" ||
    typeof t.attempts !== "number" ||
    typeof t.bodyDoc !== "string" ||
    typeof t.title !== "string" ||
    typeof t.project !== "string"
  ) return false
  if (
    t.bodyUpdated !== undefined && typeof t.bodyUpdated !== "string"
  ) return false
  if (t.agentActive !== undefined && typeof t.agentActive !== "boolean") return false
  const sesOk = t.sessionID === "" || (typeof t.sessionID === "string" && t.sessionID.startsWith("ses_"))
  if (!sesOk) return false
  if (t.worktree === "" && t.branch === "") return true
  return (
    typeof t.worktree === "string" && t.worktree.startsWith("/") &&
    typeof t.branch === "string" && t.branch.startsWith("work/")
  )
}
