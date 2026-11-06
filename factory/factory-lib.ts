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
  /** сессионные ячейки строки («Сессия dev»/«Сессия ревью»): непустые —
   *  фабрика спавнила задачу раньше (свидетель спавна, переживает потерю
   *  state); "" — свидетельства нет (легитимно новая строка) */
  devSession?: string
  reviewSession?: string
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
  /** корневой каталог окружения задачи (<root>/<row-id>); undefined —
   *  env-механика к задаче не применялась */
  envDir?: string
  /** задача доживает в старом поведении (cwd = cfg.local, агент сам делает
   *  ворктри): штамп loadState для записей, спавненных до env-механики */
  legacy?: boolean
}

export interface FactoryState {
  tasks: Record<string, TrackedTask>
  /** ISO последнего алерта эпизода гарда спавна (HOLD каскада); undefined —
   *  эпизода нет. Снимает демон: verdict=allow при стоящем флаге → удалить
   *  (зеркало сброса stallSignaled — без сброса следующий эпизод остался бы
   *  без алерта и бэкапа навсегда) */
  holdSignaled?: string
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

/** Короткий текст ошибки для сводок (лог полного стека — не её дело). */
export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

// ——— фоллбек тела по тайтлу: путь доски, мемоизированный на проход ———

/** Итог обращения за путём доски: active — путь получен; failed — get_doc
 *  упал (reason — текст ошибки); disabled — ядро вернуло пустые box/hpath
 *  (SQL-лаг индекса по доске), фоллбек на проход невозможен. */
export type BoardPathResolution =
  | { kind: "active"; box: string; hpath: string }
  | { kind: "failed"; reason: string }
  | { kind: "disabled" }

/** Ленивый мемоизер пути доски на один проход readTasks: первая
 *  материализация зовёт load и логирует деградацию ровно одной строкой,
 *  последующие вызовы отдают кэш без API-вызовов и без логов — при сбое
 *  или SQL-лаге N detached-строк дают один вызов и один лог вместо N
 *  (per-row catch фоллбека на per-row сбои hpath больше не реагирует).
 *  Живёт в замыкании readTasks: следующий тик материализует заново —
 *  самолечение через тики сохранено. */
export function createBoardPathOnce(
  load: () => Promise<{ box?: string; hpath?: string }>,
  log: (m: string) => void,
): () => Promise<BoardPathResolution> {
  let cache: BoardPathResolution | null = null
  return async () => {
    if (cache !== null) return cache
    try {
      const d = await load()
      const box = d.box ?? ""
      const hpath = d.hpath ?? ""
      if (box === "" || hpath === "") {
        log("фоллбек тела по тайтлу отключён (SQL-лаг): get_doc доски вернул пустые box/hpath")
        cache = { kind: "disabled" }
      } else {
        cache = { kind: "active", box, hpath }
      }
    } catch (e) {
      const reason = errText(e)
      log(`фоллбек тела по тайтлу отключён на проход: get_doc доски не удался (${reason})`)
      cache = { kind: "failed", reason }
    }
    return cache
  }
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

/** Контекст окружения задачи для промптов: пути приходят из ensureEnv.
 *  undefined — env-механика не применяется (легаси-штамп или env:false-проект),
 *  агент действует по старым строкам (сам создаёт ворктри/ветку). */
export interface SpawnEnvContext {
  repoDir: string
  branch: string
  scratchDir: string
}

/** Стартовый промпт будильника: пайп живёт в AGENTS.md (глобально),
 *  демон знает только «какая задача» и «где работать». */
export function spawnPrompt(
  rowId: string, project: string, repo: string, mode: KickoffMode = "design", env?: SpawnEnvContext,
): string {
  const stage = mode === "implement"
    ? [
        "Статус In Progress — владелец форсировал старт (fast-track): стадию Design",
        "и «Ревью плана» пропускаешь. Если в теле ещё нет плана — короткий план",
        "первой секцией тела (без план-ревью) и сразу в работу:",
      ]
    : [
        "Стадия Design: изучи задачу/репо, утряси вопросы с юзером (комменты, статус Needs attention",
        "пока ждёшь ответа). Перед планом — скан территории: uwuwu-cli task list на смежные задачи",
        "+ gh pr list --state all (батч-сводки/hygiene съедают срезы планов тихо).",
        "План утрясан → план в тело → статус «Ревью плана» → конец хода:",
        "ревью проводит отдельный праймари-агент фабрики, вердикт придёт wakeup'ом.",
        "После approve плана:",
      ]
  const workLine = env === undefined
    ? ["создай worktree+ветку от origin/master (имя по сути задачи, не id), заполни Branch-ячейку и реализуй."]
    : [
        "работай в своём окружении задачи (фабрика создала, ветка уже от origin/master):",
        `repo=${env.repoDir}, ветка=${env.branch}, скретч=${env.scratchDir} (не /tmp).`,
        "Branch/Path-ячейки и сессии заполняет фабрика — тебе их трогать не нужно.",
      ]
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}).`,
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей»: uwuwu-cli task get — тело, тип, комменты.",
    ...stage,
    ...workLine,
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
export function continuePrompt(rowId: string, project: string, repo: string, env?: SpawnEnvContext): string {
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}): сессия разработки перезапущена.`,
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей». Перечитай uwuwu-cli task get —",
    "тело, комменты и свой прошлый прогресс. Стадию определи по текущему статусу и телу,",
    "продолжай с места, где остановился (если работа уже идёт — не начинай её заново).",
    ...(env !== undefined
      ? [
          `Окружение задачи уже существует — своё не создавай: repo=${env.repoDir}, ветка=${env.branch},`,
          `скретч=${env.scratchDir} (не /tmp). Незакоммиченное — там же, в repo.`,
        ]
      : []),
    "",
    "ВАЖНО: юзер НЕ читает эту сессию — вся коммуникация только в uwuwu:",
    "вопросы — task comment, результаты и выводы — task body; ждёшь ответа юзера →",
    "task edit --status \"Needs attention\" и завершай ход. Ответ придёт комментом — тебя разбудят.",
  ].join("\n")
}

/** Конфликт-kickoff: PR ветки задачи конфликтует с master (владелец флипнул
 *  статус в In Progress — «поправь», или ревью вернуло задачу при конфликте).
 *  env называет существующее окружение задачи — агент не плодит новое. */
export function conflictFixPrompt(rowId: string, pr: string, env?: SpawnEnvContext): string {
  return [
    `[factory] Задача ${rowId}: PR ветки конфликтует с master (${pr}).`,
    "",
    ...(env !== undefined
      ? [
          `Окружение задачи уже существует — своё не создавай: repo=${env.repoDir}, ветка=${env.branch},`,
          `скретч=${env.scratchDir} (не /tmp). Незакоммиченное — там же, в repo.`,
          "",
        ]
      : []),
    "Почини конфликт: git fetch origin, обнови ветку от master (rebase/merge — как принято в репо),",
    "разреши конфликты, сохранив смысл обеих сторон, прогони self-check (build/lint/test), запушь.",
    "Убедись, что PR стал mergeable (gh pr view <pr> --json mergeable), затем верни задачу",
    `в «${REVIEW_RESULT_STATUS}» (uwuwu-cli task edit --status) и заверши ход.`,
  ].join("\n")
}

/** Вид старта сессии девелопера: свежий спавн (режим от kickoffModeOf),
 *  рестарт умершей сессии (continue, стадию не предписываем) или
 *  конфликт-рестарт (conflict-fix: PR ветки в конфликте с master). */
export type StartKind = KickoffMode | "continue" | "conflict-fix"

/** Единственная точка выбора стартового промпта по виду старта.
 *  pr нужен конфликт-режиму (конфликт-kickoff ссылается на PR); env
 *  сквозной для всех веток — рестарт env-задачи называет окружение. */
export function promptForStart(
  start: StartKind, rowId: string, project: string, repo: string, pr: string,
  env?: SpawnEnvContext,
): string {
  if (start === "continue") return continuePrompt(rowId, project, repo, env)
  if (start === "conflict-fix") return conflictFixPrompt(rowId, pr, env)
  return spawnPrompt(rowId, project, repo, start, env)
}

/** mergeable-статус PR из вывода `gh pr view <pr> --json mergeable`.
 *  Не-JSON, неожиданные значения → UNKNOWN (трактуется как «не конфликт»). */
export function prMergeableOf(raw: string): "CONFLICTING" | "MERGEABLE" | "UNKNOWN" {
  try {
    const j = JSON.parse(raw) as { mergeable?: unknown }
    if (j.mergeable === "CONFLICTING" || j.mergeable === "MERGEABLE") return j.mergeable
    return "UNKNOWN"
  } catch {
    return "UNKNOWN"
  }
}

/** Шаг опроса mergeable. GitHub вычисляет конфликтность лениво: первый
 *  запрос по «холодному» PR возвращает UNKNOWN и лишь запускает фоновое
 *  вычисление — значение появляется на повторных запросах через секунды.
 *  CONFLICTING/MERGEABLE — финальные вердикты; UNKNOWN переспрашивается,
 *  пока не исчерпаны попытки, затем — мягкая деградация («не конфликт»). */
export type MergeablePollStep =
  | { t: "conflict" }
  | { t: "clear" }
  | { t: "retry" }
  | { t: "degrade" }

/** Единственная точка решения «что дальше» на попытке опроса mergeable. */
export function mergeablePollStep(
  mergeable: "CONFLICTING" | "MERGEABLE" | "UNKNOWN",
  attempt: number,
  maxAttempts: number,
): MergeablePollStep {
  if (mergeable === "CONFLICTING") return { t: "conflict" }
  if (mergeable === "MERGEABLE") return { t: "clear" }
  return attempt + 1 < maxAttempts ? { t: "retry" } : { t: "degrade" }
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
    ...(ctx.worktree !== ""
      ? [`   Ворктри задачи: ${ctx.worktree}`]
      : kind === "result"
        ? [`   Ворктри задачи не найден — ревью по diff origin/master…${ctx.branch || "<ветка из Branch-ячейки>"} / PR`,
           `   (git-команды из каталога сессии; ворктри не обязателен для диффа).`]
        : []),
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
    "- владелец вернул в Design → перепланируй и снова «Ревью плана»;",
    "- владелец перевёл в In Progress (fast-track) → продолжай/почини по текущей фазе, Design не нужен.",
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
 *  девелоперу (из ревью-статуса по вердикту, ручной перевод в Design или
 *  fast-track флип владельца в In Progress — «вмешайся и почини») → noop. */
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
  // fast-track трекенной задачи: перевод владельца в In Progress из любого
  // другого статуса — «вмешайся и почини» (конфликт PR, зависший раунд,
  // форсаж Design-задачи). lastStatus "" — свежий карантин без спавна:
  // перевода не было, будит только коммент юзера.
  if (f.status === "In Progress" && f.lastStatus !== "In Progress" && f.lastStatus !== "") {
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
    "Напиши комментарий в задачу, переведи строку в Design (перепланировка) или в In Progress (fast-track «поправь») — любой из путей будит агента.",
  ].join(" ")
  return { t: "signal", text }
}

// ——— гард спавн-волны: рубеж каскада при потере/порче state (инцидент 06.11) ———

export const SPAWN_LOSS_MAX_DEFAULT = 2
export const SPAWN_BURST_MAX_DEFAULT = 12
/** окно повтора алерта при длящемся HOLD (анти-спам: между повторами — только лог и сводка) */
export const SPAWN_ALERT_REPEAT_MS = 60 * 60_000

export interface SpawnGuardLimits {
  /** сирот-спавнов, переживаемых без алерта (пара переоткрытых старых Done-строк) */
  lossMax: number
  /** рубеж каскада: untracked spawnable сверх этого — HOLD на тик (не лимит обычной волны) */
  burstMax: number
}

export interface SpawnGuardFacts {
  /** untracked spawnable с непустой сессионной ячейкой — «сироты-спавны»:
   *  спавн был, трека нет — сигнатура потери state */
  orphaned: number
  /** всего untracked spawnable (сироты + легитимно новые) */
  totalUntracked: number
  /** ISO последнего алерта эпизода (state.holdSignaled); undefined — эпизода нет */
  holdSignaled?: string
  now: number
}

export type SpawnGuardAlert = "first" | "repeat" | "none"

export type SpawnGuardHold = {
  t: "hold"
  reason: "state-loss" | "burst"
  alert: SpawnGuardAlert
  text: string
}

export type SpawnGuardVerdict = { t: "allow" } | SpawnGuardHold

/** UWUWU_FACTORY_SPAWN_LOSS_MAX / UWUWU_FACTORY_SPAWN_BURST_MAX: пусто →
 *  дефолты 2/12; мусор/0/отрицательные → дефолт. Семантики «0 = выключено»
 *  нет — отключение гарда только заведомо большим значением (9999). */
export function spawnLimitsFromEnv(loss: string | undefined, burst: string | undefined): SpawnGuardLimits {
  const parse = (v: string | undefined, fallback: number): number => {
    if (v === undefined || v.trim() === "") return fallback
    const n = Number(v)
    return Number.isFinite(n) && n > 0 ? n : fallback
  }
  return { lossMax: parse(loss, SPAWN_LOSS_MAX_DEFAULT), burstMax: parse(burst, SPAWN_BURST_MAX_DEFAULT) }
}

/** Единственная точка решения гарда (по образцу routeTask/stallVerdict):
 *  превышение любого порога = HOLD спавнов на тик. reason=state-loss, когда
 *  сигнатура потери (сироты-спавны > lossMax); иначе burst. alert=first —
 *  вход в эпизод (флага нет или битый ISO), repeat — длящийся HOLD за окном
 *  повтора, none — в пределах окна (анти-спам). allow при стоящем флаге —
 *  сигнал демону снять флаг: эпизод завершён. */
export function spawnGuardVerdict(f: SpawnGuardFacts, l: SpawnGuardLimits): SpawnGuardVerdict {
  if (f.orphaned <= l.lossMax && f.totalUntracked <= l.burstMax) return { t: "allow" }
  const reason = f.orphaned > l.lossMax ? "state-loss" as const : "burst" as const
  const prevMs = f.holdSignaled !== undefined ? Date.parse(f.holdSignaled) : Number.NaN
  const alert: SpawnGuardAlert = Number.isNaN(prevMs)
    ? "first"
    : f.now - prevMs >= SPAWN_ALERT_REPEAT_MS ? "repeat" : "none"
  const counts = `сирот-спавнов=${f.orphaned} (порог ${l.lossMax}), untracked=${f.totalUntracked} (порог ${l.burstMax})`
  const text = reason === "state-loss"
    ? `[factory] Спавн задержан — подозрение на потерю state.json: на доске строки с сессионными ячейками, а трека в state нет (${counts}). ` +
      "Восстанови state из бэкапа (state.json.bak / bak-suspect-* рядом с state.json) — каскад снимется сам. " +
      "Если волна ожидаема — подними UWUWU_FACTORY_SPAWN_LOSS_MAX / UWUWU_FACTORY_SPAWN_BURST_MAX и перезапусти демон."
    : `[factory] Спавн задержан — за один тик набралось untracked=${f.totalUntracked} при пороге ${l.burstMax} ` +
      `(сирот-спавнов=${f.orphaned}): массовый импорт задач или потеря state без свидетелей-ячеек. ` +
      "Если волна ожидаема — подними UWUWU_FACTORY_SPAWN_BURST_MAX и перезапусти демон; иначе проверь state.json."
  return { t: "hold", reason, alert, text }
}

/** Адресат алерта — фолбэк-цепочка по порядку скана доски: сирота с телом →
 *  первая строка с телом (чистый burst без сирот) → null (ни одной строки
 *  с телом — алерт недоставим, терминальный лог-only случай). */
export function alertTargetOf(candidates: TaskRef[]): TaskRef | null {
  const orphanWithBody = candidates.find(
    (r) => r.bodyDoc !== "" && (r.devSession !== "" || r.reviewSession !== ""),
  )
  if (orphanWithBody !== undefined) return orphanWithBody
  return candidates.find((r) => r.bodyDoc !== "") ?? null
}

/** Ячейки доски, которые заполняет фабрика. Колонки добавляет владелец;
 *  запись best-effort (нет колонки → тихий no-op ядра, работа не ломается). */
export const PATH_CELL = "Path"
export const DEV_SESSION_CELL = "Сессия dev"
export const REVIEW_SESSION_CELL = "Сессия ревью"

/** Штамп перехода: записи state, спавненные до env-механики, доживают в
 *  старом поведении (рестарт не уводит незакоммиченную работу в новый env).
 *  envDir отсутствует И сессия была спавнена → легаси; карантинные записи
 *  без сессии не штампуются — их спавн ещё попробует env-механику. */
export function stampLegacyEntries(state: FactoryState): string[] {
  const stamped: string[] = []
  for (const [rowId, tracked] of Object.entries(state.tasks)) {
    if (tracked.envDir !== undefined || tracked.legacy === true) continue
    if (tracked.sessionID === "") continue
    tracked.legacy = true
    stamped.push(rowId)
  }
  return stamped
}

/** Валидация записи state.json. Легальные форматы: полный (ses_ + пути),
 *  чистый карантин (всё пусто) и гибрид (сессия умерла, пути живы — работа
 *  сохраняется до решения юзера). worktree = repoDir env-задачи; branch —
 *  производное имя ветки (пишет фабрика) или легаси-имя агента. */
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
  if (t.envDir !== undefined && !(typeof t.envDir === "string" && t.envDir.startsWith("/"))) return false
  if (t.legacy !== undefined && typeof t.legacy !== "boolean") return false
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
  /** env-механика задачи (worktree+скретч в ~/agents-env). false — проект
   *  вне механики (cfg.local не git-репо → контракт невыполним): легаси-
   *  поведение, агент сам делает ворктри/ветку. Дефолт true. */
  env?: boolean
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
    if (c.env !== undefined && typeof c.env !== "boolean") {
      throw new Error(`UWUWU_FACTORY_PROJECTS_JSON: проект «${name}» — env должен быть boolean`)
    }
    out[name] = { board: c.board, repo: c.repo, local: c.local, ...(c.env !== undefined ? { env: c.env } : {}) }
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
  /** окружения задачи: создано/снесено (rm) / в attic за тик */
  envCreated: number
  envRemoved: number
  envAtticked: number
  /** задачи под наблюдением на конец прохода (этот проект) */
  watched: number
  /** спавны, задержанные гардом каскада за тик (HOLD); undefined — гарда не было */
  held?: number
  /** причина HOLD для сводки; meaningful только при held */
  heldReason?: "state-loss" | "burst"
  /** фейлы спавна: короткие «rowId-префикс: причина» */
  failures: string[]
}

/** Одна строка лога на проект за тик: «молчаливые тики» должны быть
 *  видимы — что сделано и что живо (прод-кейс 05.11). */
export function formatPassSummary(project: string, c: PassCounters): string {
  const failures = c.failures.length > 0 ? `, фейлы=${c.failures.length} (${c.failures.join("; ")})` : ""
  const envPart = `, env=+${c.envCreated}/-${c.envRemoved}/attic:${c.envAtticked}`
  const heldPart = c.held !== undefined && c.held > 0
    ? `, задержано гардом=${c.held}${c.heldReason !== undefined ? ` (${c.heldReason})` : ""}`
    : ""
  return `проход ${project}: спавн dev=${c.devSpawned} ревью=${c.reviewSpawned}` +
    `, будильники=${c.wakeups}, чистки=${c.cleanups}${envPart}${heldPart}, наблюдение=${c.watched}${failures}`
}
