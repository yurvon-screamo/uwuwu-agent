// factory-lib.ts — чистые функции uwuwu-factory: состояние, комменты, промпты.
// Задача = строка SiYuan database (rowBlockId) + саб-док-тело.
// Старые комменты (до бот-аккаунта) не несли автора — фоллбек-фильтр по маркеру 🤖/[factory].

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
  /** автор коммента (user.id из афины) — точный фильтр юзер/бот */
  userId?: string
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
  /** последний наблюдённый статус — граница edge-триггеров (Design-рестарт) */
  lastStatus?: string
  /** проект (ключ конфига демона) */
  project: string
  /** ms-время последнего обработанного коммента */
  lastCommentAt: number
  /** id последнего обработанного коммента (тай-брейк в одну мс) */
  lastCommentId: string
  createdAt: number
  attempts: number
  /** сессия ревьювера (1 на задачу; "" — нет или умерла) */
  reviewSessionID?: string
  /** неудачные спавны ревьювера подряд (эскалация до Needs attention) */
  reviewAttempts?: number
  /** ISO первой пропажи строки из скана доски (suspect); undefined —
   *  строка наблюдалась. Чистка finishTask необратима (delete state +
   *  DELETE ревью-сессии) — только при пропаже в двух проходах подряд:
   *  одиночный промах скана не должен терять трекинг (прод-кейс 05.11). */
  missingSince?: string
  /** сигнал о застое уже отправлен в этом эпизоде In Progress (анти-спам
   *  «раз за эпизод»; сброс делает демон: смена статуса — штатный путь,
   *  none-вердикт при стоявшем флаге — эпизод завершён) */
  stallSignaled?: boolean
}

export interface FactoryState {
  tasks: Record<string, TrackedTask>
}

/** Комменты агента — с этим префиксом, чтобы не петлить себя. */
export const AGENT_MARK = "🤖"

// ——— ревью-статусы фабрики: имена = контракт со Status-колонками досок ———

export const REVIEW_PLAN_STATUS = "Ревью плана"
export const REVIEW_RESULT_STATUS = "Ревью результата"

export type ReviewKind = "plan" | "result"

/** Ворктри задачи по Branch-ячейке из `git worktree list --porcelain`.
 *  Точное сравнение refs/heads/<branch> — суффикс-матч ловил чужие ветки
 *  (commits совпадал с auto-atomic-commits). */
export function worktreeByBranch(porcelain: string, branch: string): string {
  if (branch === "") return ""
  let worktree = ""
  for (const line of porcelain.split("\n")) {
    if (line.startsWith("worktree ")) worktree = line.slice("worktree ".length)
    else if (line === `branch refs/heads/${branch}` && worktree !== "") return worktree
  }
  return ""
}

/** Ревью-статус → вид ревью; null — обычный статус (мяч у девелопера/юзера). */
export function reviewKindOf(status: string): ReviewKind | null {
  if (status === REVIEW_PLAN_STATUS) return "plan"
  if (status === REVIEW_RESULT_STATUS) return "result"
  return null
}

/** Юзерский коммент: не наш (🤖), не служебный ([factory]). */
export function isUserComment(c: TaskComment, botUserId?: string): boolean {
  // точный фильтр: комменты бот-аккаунта — не юзер (независимо от текста)
  if (botUserId !== undefined && c.userId !== undefined && c.userId === botUserId) {
    return false
  }
  const body = c.text.trimStart()
  if (body.startsWith(AGENT_MARK)) return false
  if (body.startsWith("[factory]")) return false
  return body.trim() !== ""
}

/** Самоисцеление трека: пустой bodyDoc в state (след спавна до-фиксного
 *  кода или сбоя маппинга) принимает итоговый резолв строки — трек снова
 *  видит комменты/правки тела. Вход — row.bodyDoc (пост-fallback readTasks,
 *  единственная точка резолва: тик не дублирует логику). Непустой трек не
 *  перезаписывается — state выигрывает, adopt идемпотентен. */
export function adoptBodyDoc(tracked: TrackedTask, row: TaskRef): string | null {
  if (tracked.bodyDoc === "" && row.bodyDoc !== "") return row.bodyDoc
  return null
}

/** Тайтл строки → безопасный сегмент hpath саб-дока доски (паритет CLI
 *  path_safe: '/' → U+2215 — SiYuan трактует '/' как разделитель сегментов;
 *  конвенция миграции bind_rows.py). */
export function pathSafe(title: string): string {
  return title.replaceAll("/", "\u{2215}")
}

/** Новые юзерские комменты после lastSeen, по порядку создания. */
export function newUserComments(
  comments: TaskComment[],
  lastSeen: { at: number; id: string },
  botUserId?: string,
): TaskComment[] {
  const users = comments
    .filter((c) => isUserComment(c, botUserId))
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

/** Текст коммента с контекстом ответа: page-режим (ответ цитатой на кусок
 *  дока/коммента) несёт preview = цитата-якорь; треды — см. taskComments. */
export function commentText(content: unknown): string {
  const c = content as { mode?: string; preview?: string } | null
  const body = snapshotText(content)
  if (
    c !== null && typeof c === "object" &&
    c.mode === "page" && typeof c.preview === "string" &&
    c.preview !== "" && c.preview !== body
  ) {
    return `(в ответ на: «${c.preview.slice(0, 120)}») ${body}`
  }
  return body
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

/** Режим стартового промпта: design → утряска с план-ревью,
 *  implement → fast-track (стадия реализации, без план-ревью). */
export type KickoffMode = "design" | "implement"

/** Режим агента по статусу строки: In Progress = стадия реализации
 *  (fast-track владельца или approve плана ревьювером) → implement;
 *  всё остальное → design (safe default). Единственная точка решения
 *  «какой агент родится» — чистая, по образцу routeTask. */
export function kickoffModeOf(status: string): KickoffMode {
  return status === "In Progress" ? "implement" : "design"
}

/** Стартовый промпт будильника: пайп живёт в AGENTS.md (глобально),
 *  демон знает только «какая задача» и «где работать». */
export function spawnPrompt(rowId: string, project: string, repo: string, mode: KickoffMode = "design"): string {
  const stage = mode === "implement"
    ? [
        "Статус In Progress — владелец форсировал старт (fast-track): стадию Design",
        "и «Ревью плана» пропускаешь. Если в теле ещё нет плана — короткий план",
        "первой секцией тела (без план-ревью) и сразу в работу:",
      ]
    : [
        "Стадия Design: изучи задачу/репо, утряси вопросы с юзером (комменты, статус Needs attention",
        "пока ждёшь ответа). План утрясан → план в тело → статус «Ревью плана» → конец хода:",
        "ревью проводит отдельный праймари-агент фабрики, вердикт придёт wakeup'ом.",
        "После approve плана:",
      ]
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}).`,
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей»: uwuwu-cli task get — тело, тип, комменты.",
    ...stage,
    "создай worktree+ветку от origin/master (имя по сути задачи, не id), заполни Branch-ячейку и реализуй.",
    "Код + self-check + PR (gh pr create, URL в PR-ячейку) + отчёт в тело → статус",
    "«Ревью результата» → конец хода. Вернулся wakeup'ом с вердиктом: request_changes →",
    "устрани замечания и снова ревью-статус. Ревьювера сам не вызываешь.",
    "Без пустого шума в комментах («взялся», «отчёт») — коммент только вопрос юзеру или PR-ссылка.",
    "",
    "ВАЖНО: юзер НЕ читает эту сессию — вся коммуникация только в uwuwu:",
    "вопросы — task comment, результаты и выводы — task body; ждёшь ответа юзера →",
    "task edit --status \"Needs attention\" и завершай ход. Ответ придёт комментом — тебя разбудят.",
  ].join("\n")
}

/** Рестарт-промпт (сессия девелопера умерла/карантин → fresh spawn).
 *  Стадию НЕ предписываем: fast-track в In Progress продолжит реализацию,
 *  Design-задача — утряску; фаза живёт в теле задачи и текущем статусе. */
export function continuePrompt(rowId: string, project: string, repo: string): string {
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}): сессия разработки перезапущена.`,
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей». Перечитай uwuwu-cli task get —",
    "тело, комменты и свой прошлый прогресс. Стадию определи по текущему статусу и телу,",
    "продолжай с места, где остановился (если работа уже идёт — не начинай её заново).",
    "",
    "ВАЖНО: юзер НЕ читает эту сессию — вся коммуникация только в uwuwu:",
    "вопросы — task comment, результаты и выводы — task body; ждёшь ответа юзера →",
    "task edit --status \"Needs attention\" и завершай ход. Ответ придёт комментом — тебя разбудят.",
  ].join("\n")
}

/** Будильник: юзер отредактировал ТЕЛО задачи (не коммент). */
export function bodyEditPrompt(rowId: string): string {
  return [
    `[factory] Юзер отредактировал ТЕЛО задачи ${rowId} (не коммент).`,
    "",
    "Перечитай тело (uwuwu-cli task get — печатает тело целиком), найди что изменилось,",
    "учти изменения в работе. Ответь/доложи по пайпу — комментом.",
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
    "Отреагируй по пайпу: ответь/исправь. Ответ пользователю — uwuwu-cli task comment, не текст в сессии.",
  ].join("\n")
}

// ——— промпты ревьювера (праймари-агент на ревью-статусах) ———

export interface ReviewContext {
  branch: string
  worktree: string
  pr: string
}

/** Стартовый промпт ревьювера: фабрика спавнит сессию на ревью-статусе. */
export function reviewSpawnPrompt(rowId: string, project: string, kind: ReviewKind, ctx: ReviewContext): string {
  const lines = [
    `[factory] Задача ${rowId} (проект ${project}): ревью ${kind === "plan" ? "плана" : "результата"}.`,
    "",
    "Ты — праймари-агент code-quality-reviewer, фабрика назначила тебя судьёй этой задачи",
    "(1 задача = 1 твоя сессия; повторные раунды правок придут сюда же wakeup'ом).",
    "Работай по секции «Ревью задач фабрики uwuwu» своей методики:",
    "",
    `1. uwuwu-cli task get ${rowId} --project ${project} — тело задачи и треды комментов.`,
    kind === "plan"
      ? "2. Вид plan → review_type=plan: план в теле задачи; кодовый diff не нужен."
      : "2. Вид result → review_type=full, источник branch: все изменения ветки против",
    ...(kind === "result"
      ? ["   origin/master + untracked; PR (gh pr diff/view, CI) — дополнительные артефакты.",]
      : []),
    ...(ctx.worktree !== "" ? [`   Ворктри задачи: ${ctx.worktree}`] : []),
    ...(ctx.branch !== "" ? [`   Ветка: ${ctx.branch}`] : []),
    ...(ctx.pr !== "" ? [`   PR: ${ctx.pr}`] : []),
    "3. Отчёт — комментом в задачу (uwuwu-cli task comment), стандартный формат,",
    "   вердикт-строка «Статус: approve|request_changes» обязательна.",
    "4. Статус после вердикта (uwuwu-cli task edit):",
    kind === "plan"
      ? "   approve → In Progress · request_changes → Design"
      : "   approve → Review · request_changes → In Progress",
    "5. Вопросы юзеру — комментом без смены статуса; код и тело задачи не правь.",
  ]
  return lines.join("\n")
}

/** Будильник ревьювера: девелопер завершил раунд, статус снова ревью-статус. */
export function reviewRerunPrompt(rowId: string, kind: ReviewKind, ctx: ReviewContext): string {
  const status = kind === "plan" ? REVIEW_PLAN_STATUS : REVIEW_RESULT_STATUS
  return [
    `[factory] Задача ${rowId}: девелопер завершил раунд — статус снова «${status}».`,
    "",
    "Доревьюируй в этой же сессии: перечитай задачу (uwuwu-cli task get + треды),",
    "проверь устранение своих прошлых замечаний, отчёт комментом,",
    `статус по вердикту (${kind === "plan" ? "approve → In Progress · request_changes → Design" : "approve → Review · request_changes → In Progress"}).`,
    ...(ctx.pr !== "" ? [`PR: ${ctx.pr}`] : []),
    ...(ctx.worktree !== "" ? [`Ворктри: ${ctx.worktree}`] : []),
  ].join("\n")
}

/** Будильник ревьювера: свежие комменты юзера в ревью-статусе. */
export function reviewCommentsPrompt(rowId: string, comments: TaskComment[]): string {
  const quoted = comments.map((c) => c.text.trim()).join("\n---\n")
  return [
    `[factory] В задаче ${rowId} (ревью-статус) новые комментарии от пользователя:`,
    "",
    quoted,
    "",
    "Ответь/учти по своему протоколу ревью: ответ юзеру — uwuwu-cli task comment.",
  ].join("\n")
}

/** Будильник девелоперу: задача вернулась к разработке (вердикт ревьювера
 *  или ручной перевод владельца в Design). */
export function reviewReturnPrompt(rowId: string): string {
  return [
    `[factory] Задача ${rowId} вернулась к разработке — статус сменил ревьювер (вердикт) или владелец.`,
    "",
    "Перечитай uwuwu-cli task get + треды и действуй по пайпу:",
    "- есть замечания ревьювера → устрани, запушь, снова поставь ревью-статус;",
    "- план approved → реализуй;",
    "- результат approved → мяч у юзера, от тебя ничего не требуется;",
    "- владелец вернул в Design → перепланируй и снова «Ревью плана».",
  ].join("\n")
}

// ——— маршрутизация: одно решение на тик (чистая функция для тестов) ———

export interface RouteFacts {
  /** статус строки сейчас */
  status: string
  /** последний наблюдённый статус (граница edge-триггеров) */
  lastStatus: string
  /** жива ли сессия ревьювера (null — liveness неизвестна: тик пропускаем решением noop) */
  reviewerAlive: boolean | null
  /** свежие комменты юзера */
  freshComments: TaskComment[]
}

export type RouteAction =
  | { t: "spawn-reviewer"; kind: ReviewKind }
  | { t: "rerun-reviewer"; kind: ReviewKind }
  | { t: "comments-to-reviewer" }
  | { t: "return-to-dev" }
  | { t: "noop" }

/** Решение по задаче на один тик. Приоритет порядка: спавн ревьювера →
 *  доревью (вход/смена вида ревью) → комменты юзера ревьюверу → возврат
 *  девелоперу (из ревью-статуса или ручной перевод в Design) → noop. */
export function routeTask(f: RouteFacts): RouteAction {
  const kind = reviewKindOf(f.status)
  if (kind !== null) {
    // null (liveness неизвестна) трактуем как «жива»: не плодим спавны на
    // транзиентных ошибках REST — следующий тик перепроверит
    if (f.reviewerAlive === false) return { t: "spawn-reviewer", kind }
    if (reviewKindOf(f.lastStatus) !== kind) return { t: "rerun-reviewer", kind }
    if (f.freshComments.length > 0) return { t: "comments-to-reviewer" }
    return { t: "noop" }
  }
  // возврат мяча девелоперу: из ревью-статуса по вердикту ревьювера…
  if (reviewKindOf(f.lastStatus) !== null &&
      (f.status === "Design" || f.status === "In Progress")) {
    return { t: "return-to-dev" }
  }
  // …или ручной перевод живой задачи в Design («перепланируй»)
  if (f.status === "Design" && f.lastStatus !== "Design" && f.lastStatus !== "") {
    return { t: "return-to-dev" }
  }
  return { t: "noop" }
}

export type RowMissingAction = "mark" | "clean" | "clear" | "hold"

/** Пропажа строки трека из скана доски. rowFound=false: первая пропажа —
 *  suspect-пометка (mark), повторная — чистка (clean, необратимая
 *  finishTask). rowFound=true: стоявшая пометка снимается (clear), без
 *  пометки — ничего (hold). Два прохода подряд страхуют необратимую чистку
 *  от одиночного промаха скана (дрейф страниц при конкурентных правках). */
export function rowMissingAction(missingSince: string | undefined, rowFound: boolean): RowMissingAction {
  if (rowFound) return missingSince === undefined ? "hold" : "clear"
  return missingSince === undefined ? "mark" : "clean"
}

// ——— сигнал о застое: In Progress, сессия жива, агент молчит ———

/** Чистый парсер UWUWU_FACTORY_STALL_MIN: undefined/пусто → 30; конечное
 *  число > 0 → оно; иначе (NaN, 0, отрицательное, мусор) → дефолт 30.
 *  0 исключён — сигнал срабатывал бы на каждой паузе между сообщениями
 *  (анти-спам не спасёт), NaN исключён — мёртвая фича. */
export function stallMinutesFromEnv(v: string | undefined): number {
  if (v === undefined || v.trim() === "") return 30
  const n = Number(v)
  return Number.isFinite(n) && n > 0 ? n : 30
}

/** Активность сессии по последнему сообщению (список DESC — берём только
 *  [0]): max числовых полей time (created/streamed/completed). null —
 *  сообщений нет (= застой: трекнутая сессия минимум с промптом-сообщением).
 *  "invalid" — ответ не распознаётся (пропуск тика, без ложных сигналов).
 *  Синтетический idle-маркер и сообщение без completed валидны: достаточно
 *  одного числового поля времени; отсутствие completed — норма. */
export function lastActivityOf(msgs: unknown[]): number | null | "invalid" {
  if (msgs.length === 0) return null
  const m = msgs[0] as { time?: unknown } | null
  if (m === null || typeof m !== "object") return "invalid"
  if (m.time === null || typeof m.time !== "object") return "invalid"
  const t = m.time as Record<string, unknown>
  const nums = [t.created, t.streamed, t.completed].filter(
    (x): x is number => typeof x === "number" && Number.isFinite(x),
  )
  if (nums.length === 0) return "invalid"
  return Math.max(...nums)
}

export interface StallFacts {
  /** статус строки сейчас */
  status: string
  /** жива ли сессия девелопера (null — liveness неизвестна: пропуск тика) */
  sessionAlive: boolean | null
  /** активность последнего сообщения; null — сообщений нет (= застой) */
  lastActivityAt: number | null
  /** Branch-ячейка строки (деталь диагноза в тексте сигнала) */
  branch: string
  /** ветка нашлась в ворктри по `git worktree list` */
  branchInWorktree: boolean
  /** сигнал в этом эпизоде уже был (анти-спам «раз за эпизод») */
  signaled: boolean
  now: number
}

export type StallVerdict =
  | { t: "none" }
  | { t: "signal"; text: string }
  | { t: "silent" }

/** Единственная точка решения о застое (по образцу routeTask): трекнутая
 *  In Progress при живой сессии, но тишине дольше порога (или без сообщений
 *  вовсе) = потерянный/зависший ход — активный ход рождает сообщения на
 *  каждом шаге LLM. dead обрабатывает соседний код (карантин), unknown —
 *  пропуск тика: оба → none. Повторный сигнал в эпизоде глушится флагом
 *  (silent); сброс флага — дело демона: смена статуса — штатный путь
 *  (пере-вход в In Progress = новый эпизод), none-вердикт при стоявшем
 *  флаге — эпизод завершён. */
export function stallVerdict(f: StallFacts, stallMs: number): StallVerdict {
  if (f.status !== "In Progress") return { t: "none" }
  if (f.sessionAlive !== true) return { t: "none" }
  const stalled = f.lastActivityAt === null || f.now - f.lastActivityAt > stallMs
  if (!stalled) return { t: "none" }
  if (f.signaled) return { t: "silent" }
  const quietMin = f.lastActivityAt === null
    ? null
    : Math.max(1, Math.round((f.now - f.lastActivityAt) / 60_000))
  const quietText = quietMin === null ? "сообщений в сессии нет" : `тишина ${quietMin} мин`
  const branchDiag = f.branch === ""
    ? "Branch-ячейка пуста — агент умер до создания ветки"
    : f.branchInWorktree
      ? `ветка на месте (${f.branch.slice(0, 60)})`
      : `ветки нет в ворктри (${f.branch.slice(0, 60)})`
  const text = [
    `[factory] Строка в In Progress, но сессия разработки молчит: ${quietText} при пороге ${Math.round(stallMs / 60_000)} мин.`,
    `Ход потерян или завис (диагноз ветки: ${branchDiag}).`,
    "Напиши комментарий в задачу — это разбудит агента; или переведи строку в Design — фабрика перешлёт агенту промпт возврата.",
  ].join(" ")
  return { t: "signal", text }
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
  if (t.lastStatus !== undefined && typeof t.lastStatus !== "string") return false
  if (t.missingSince !== undefined && typeof t.missingSince !== "string") return false
  if (t.stallSignaled !== undefined && typeof t.stallSignaled !== "boolean") return false
  if (
    t.reviewSessionID !== undefined &&
    !(t.reviewSessionID === "" || (typeof t.reviewSessionID === "string" && t.reviewSessionID.startsWith("ses_")))
  ) return false
  if (t.reviewAttempts !== undefined && typeof t.reviewAttempts !== "number") return false
  const sesOk = t.sessionID === "" || (typeof t.sessionID === "string" && t.sessionID.startsWith("ses_"))
  if (!sesOk) return false
  if (t.worktree === "" && t.branch === "") return true
  return (
    typeof t.worktree === "string" && t.worktree.startsWith("/") &&
    typeof t.branch === "string" && t.branch.startsWith("work/")
  )
}

// ——— спавн-волна: пачка с ограничением конкурентности ———

/** Волна Design-спавна одним проходом: последовательный спавн ≈ до 75 s
 *  на задачу не влезал в тик 180 s. Ошибка одного элемента не останавливает
 *  остальных (фейл задачи — её регистрация, не обвал пачки); первая ошибка
 *  перевыбрасывается после завершения всех элементов. */
export async function runBatch<T>(items: readonly T[], worker: (item: T) => Promise<void>, limit: number): Promise<void> {
  let next = 0
  let failure: unknown = null
  const lane = async (): Promise<void> => {
    while (true) {
      const index = next++
      const item = items[index]
      if (item === undefined) return
      try {
        await worker(item)
      } catch (e) {
        failure ??= e
      }
    }
  }
  const lanes: Promise<void>[] = []
  for (let i = 0; i < Math.min(limit, items.length); i++) lanes.push(lane())
  await Promise.all(lanes)
  if (failure !== null) throw failure
}

// ——— конфиг проектов (env-оверрайд для тестовых прогонов) ———

export interface ProjectCfg {
  board: string
  repo: string
  local: string
}

/** UWUWU_FACTORY_PROJECTS_JSON: полный Record<имя, ProjectCfg> — тестовая
 *  доска/чекаут для ONCE-прогона без правки кода. Отсутствие — дефолт;
 *  невалидный — отказ старта с внятной причиной (не тихий фоллбек). */
export function parseProjects(raw: string | undefined, fallback: Record<string, ProjectCfg>): Record<string, ProjectCfg> {
  if (raw === undefined || raw.trim() === "") return fallback
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch (e) {
    throw new Error(`UWUWU_FACTORY_PROJECTS_JSON: не JSON (${e})`)
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("UWUWU_FACTORY_PROJECTS_JSON: ожидался объект { имя: { board, repo, local } }")
  }
  const out: Record<string, ProjectCfg> = {}
  for (const [name, cfg] of Object.entries(parsed as Record<string, unknown>)) {
    const c = cfg as Partial<ProjectCfg>
    if (typeof c.board !== "string" || typeof c.repo !== "string" || typeof c.local !== "string") {
      throw new Error(`UWUWU_FACTORY_PROJECTS_JSON: проект «${name}» — board/repo/local обязательны и строки`)
    }
    if (c.board.trim() === "" || c.repo.trim() === "" || c.local.trim() === "") {
      throw new Error(`UWUWU_FACTORY_PROJECTS_JSON: проект «${name}» — board/repo/local не могут быть пустыми`)
    }
    out[name] = { board: c.board, repo: c.repo, local: c.local }
  }
  if (Object.keys(out).length === 0) {
    throw new Error("UWUWU_FACTORY_PROJECTS_JSON: конфиг пуст — демон без проектов молча ничего не делает; убери env или задай проекты")
  }
  return out
}

// ——— сводка прохода ———

export interface PassCounters {
  /** спавн девелоперов (Design-утряска + fast-track In Progress) */
  devSpawned: number
  /** спавн/рестарт ревьюверов */
  reviewSpawned: number
  /** доставки промптов за тик: спавн/реран ревьювера, комменты ревьюверу,
   *  возврат девелоперу, wakeup по комментам/правкам тела */
  wakeups: number
  /** чистки state (Done / строка исчезла) */
  cleanups: number
  /** задачи под наблюдением на конец прохода (этот проект) */
  watched: number
  /** фейлы спавна: короткие «rowId-префикс: причина» */
  failures: string[]
}

/** Одна строка лога на проект за тик: «молчаливые тики» должны быть
 *  видимы — что сделано и что живо (прод-кейс 05.11). */
export function formatPassSummary(project: string, c: PassCounters): string {
  const failures = c.failures.length > 0 ? `, фейлы=${c.failures.length} (${c.failures.join("; ")})` : ""
  return `проход ${project}: спавн dev=${c.devSpawned} ревью=${c.reviewSpawned}` +
    `, будильники=${c.wakeups}, чистки=${c.cleanups}, наблюдение=${c.watched}${failures}`
}
