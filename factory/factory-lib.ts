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

/** Нейтрализация подделываемых границ в теле (промпт-инъекция). */
export function sanitizeBoundary(text: string): string {
  return text.replace(/^\s*===.*$/gm, "≡")
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

/** Промпт-контракт пайпа для новой задачи. */
export interface KickoffProfile {
  repo: string
  pipeline: string[]
  rules: string[]
  /** путь gh-обёртки бота (подставляется в правила как ${GH_AGENT}) */
  ghAgent?: string
}

/** Тип задачи (тег тела): определяет мантру пайпа. */
export type TaskKind = "dev" | "spike" | "bug"

const KIND_MANTRA: Record<TaskKind, string[]> = {
  dev: [],
  spike: [
    "ТИП: СПАЙК. Код НЕ писать (только минимальные пробы для исследования).",
    "Исследуй вопрос из тела; свои выводы доложи комментом 🤖 (правки тела — за юзером).",
    "Без PR: итог — отчёт-коммент (🤖 … [DONE]) и ЗАКРОЙ задачу: uwuwu-cli task close <rowId>.",
  ],
  bug: [
    "ТИП: БАГ. Пайп строго TDD: сначала воспроизведи по BDD-сценарию из тела (красный тест).",
    "Не воспроизвёлся — СТОП: спроси юзера комментом (🤖 детали окружения) и заверши ход.",
    "Воспроизвёлся: причина → фикс → красный тест зелёный → PR (в PR — имя теста).",
  ],
}

export function kickoffPrompt(task: TaskRef, body: string, profile: KickoffProfile, commentsHistory = "", kind: TaskKind = "dev"): string {
  return [
    `Ты — девелопер фабрики uwuwu. Репозиторий: ${profile.repo}. Задача: «${task.title.replace(/[\r\n]+/g, " ")}» (канбан-строка ${task.rowId}).`,
    ...(kind === "dev" ? [] : KIND_MANTRA[kind]),
    "",
    "=== ТЕЛО ЗАДАЧИ ===",
    sanitizeBoundary(body || "(пусто)"),
    "=== КОНЕЦ ТЕЛА ===",
    "",
    "ПАЙП (строго по порядку):",
    "1. Изучи задачу и репо (ветка ворктри уже создана — работай здесь).",
    "2. Если для старта нужны решения пользователя — задай вопросы ОДНИМ комментарием:",
    `   uwuwu-cli task comment ${task.rowId} "🤖 <вопросы>"`,
    "   (первая строка обязательно 🤖) и ЗАВЕРШИ ход. Ответ придёт комментом — тебя разбудят.",
    "3. Если всё ясно: прогони план через code-quality-reviewer сабагента, исправь замечания.",
    "4. Реализуй. Коммить атомарно. Пуш ветки — обычный git push.",
    `5. Отчёт в задачу: uwuwu-cli task comment ${task.rowId} "🤖 <отчёт по шаблону ниже>".`,
    "6. PR: gh pr create --title «<кратко>» --body-file <файл> (выжимка отчёта + ссылка на задачу:",
    `   https://affine.up.railway.app/workspace/81475705-028a-4153-a8f5-f06e402ee31e/${task.bodyDoc}).`,
    `7. СРАЗУ после PR: uwuwu-cli task edit ${task.rowId} --pr <url> --status Review --branch <ветка>.`,
    "8. Проверь CI (gh pr checks --watch может висеть — опрашивай gh pr view). Красный — чини.",
    "9. Мержит пользователь. Замечания придут комментами — исправляй в ветке и пушь.",
    ...profile.pipeline,
    "",
    "ШАБЛОН ОТЧЁТА:",
    AGENT_MARK + " Отчёт: «" + task.title + "»",
    "**Что сделано:** …",
    "**Как проверить:** … (шаги)",
    "**Риски:** …",
    "",
    "ПРАВИЛА:",
    "- КАЖДЫЙ твой комментарий начинается с " + AGENT_MARK + " — иначе разбудишь себя.",
    "- ВСЁ для пользователя — только комменты задачи (uwuwu-cli task comment). Текст в сессии юзер не читает.",
    "- Тело задачи перечитывай `uwuwu-cli task get` (печатает тело целиком).",
    "- Не мержи сам. Не закрывай задачу сам (uwuwu-cli task close — только демон по мержу PR).",
    `- Разрешено: uwuwu-cli task comment/get/edit/body, gh pr create/view/checks. Запрещено: gh pr merge.`,
    "- Тело дозаписывай: uwuwu-cli task body <rowId> --project <proj> --stdin (markdown; диаграммы — skill visual-diagrams, фенс \\`\\`\\`html_embed имя\\`)",
    ...profile.rules.map((r) => r.replace("${GH_AGENT}", profile.ghAgent ?? "gh")),
    ...(commentsHistory === "" ? [] : [
      "",
      "=== НЕДАВНИЕ КОММЕНТЫ ЗАДАЧИ (контекст, миграции/предыстория) ===",
      sanitizeBoundary(commentsHistory),
      "=== КОНЕЦ КОММЕНТОВ ===",
    ]),
  ].join("\n")
}

/** Сверка после работы агента: тело менялось, чьи правки — знает агент. */
export function mergeCheckPrompt(rowId: string): string {
  return [
    `[factory] Сверка тела задачи ${rowId}: документ менялся, пока ты работал.`,
    "",
    "Часть изменений — твои (твоя работа). Перечитай тело (uwuwu-cli task get):",
    "- есть правки, которые НЕ твои (от юзера: требования/правки/дополнения)?",
    "  → учти их в работе, ответь по пайпу комментом 🤖 что учёл;",
    "- всё твоё или ничего значимого → НИЧЕГО не делай, это штатная сверка.",
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
 *  пути живы — работа сохраняется до решения юзера). */
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
    typeof t.branch === "string" && t.branch.startsWith("work/task-")
  )
}
