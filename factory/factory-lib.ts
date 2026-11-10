import { createHash } from "node:crypto"
import { existsSync } from "node:fs"
import { join } from "node:path"

// factory-lib.ts — чистые функции uwuwu-factory: состояние, комменты, промпты.
// Задача = строка SiYuan database (rowBlockId) + саб-док-тело.
// Старые комменты (до бот-аккаунта) не несли автора — фоллбек-фильтр по маркеру 🤖/[factory].

import { sanitizeRowId } from "./env"

/** Ворктри задачи для промпт-ctx и cwd сессии. env:true-задачам путь пишет
 *  фабрика в state (tracked.worktree); env:false-проектам ворктри по канону
 *  AGENTS.md создаёт сам девелопер — ~/agents-env/<rowId>/repo — и фабрика
 *  о нём не знает: state-путь пуст навсегда, а живой каталог есть. Резолв
 *  на лету без мутации state: каталог существует и внутри .git (ворктри)
 *  → путь; иначе "" — честная фоллбэк-строка промпта («не найден»).
 *  Осиротевший .git (prune срезал регистрацию, каталог остался) — принятый
 *  компромисс против git-проб: деградация мягкая, агент увидит и уйдёт
 *  в фоллбэк сам (инциденты 08–09.11, задача 20261109133634-6ctgzew). */
export function resolvePromptWorktree(rowId: string, trackedWorktree: string, envRoot: string): string {
  if (trackedWorktree !== "") return trackedWorktree
  const repoDir = join(envRoot, sanitizeRowId(rowId), "repo")
  return existsSync(repoDir) && existsSync(join(repoDir, ".git")) ? repoDir : ""
}

export interface TaskRef {
  rowId: string
  bodyDoc: string
  title: string
  status: string
  /** тип задачи из колонки «Тип» (bug/dev/spike/analys/docs/other); "" — легаси-строка без типа */
  type: string
  pr: string
  branch: string
  /** сессионные ячейки строки («Сессия dev»/«Сессия ревью»/«Сессия тест»):
   *  непустые — фабрика спавнила задачу раньше (свидетель спавна, переживает
   *  потерю state); "" — свидетельства нет (легитимно новая строка) */
  devSession?: string
  reviewSession?: string
  testSession?: string
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
  /** сессия ревьювера (активный держатель текущего ревью-вида; "" — нет или умерла) */
  reviewSessionID?: string
  /** неудачные спавны ревьювера подряд (эскалация до Needs attention) */
  reviewAttempts?: number
  /** вид ревью, который обслуживает активная ревью-сессия ("plan"/"result");
   *  undefined — сессии нет или legacy-запись (держатель старого агента):
   *  расхождение с видом статуса трактуется как спавн нового держателя */
  reviewKind?: ReviewKind
  /** сессия тестировщика (1 на задачу; "" — нет или умерла) */
  testSessionID?: string
  /** неудачные спавны тестировщика подряд (эскалация до Needs attention) */
  testAttempts?: number
  /** ISO первой пропажи строки из скана доски (suspect); undefined —
   *  строка наблюдалась. Чистка finishTask необратима (delete state +
   *  DELETE ревью-сессии) — только при пропаже в двух проходах подряд:
   *  одиночный промах скана не должен терять трекинг (прод-кейс 05.11). */
  missingSince?: string
  /** сигнал о застое уже отправлен в этом эпизоде In Progress (анти-спам
   *  «раз за эпизод»; сброс делает демон: смена статуса — штатный путь,
   *  none-вердикт при стоявшем флаге — эпизод завершён) */
  stallSignaled?: boolean
  /** сигнал ревью-тишины уже отправлен в этом эпизоде ревью-статуса
   *  (анти-спам «раз за эпизод»; сброс — смена статуса или вернувшаяся
   *  активность ревьювера; семантика зеркальна stallSignaled) */
  reviewStallSignaled?: boolean
  /** корневой каталог окружения задачи (<root>/<row-id>); undefined —
   *  env-механика к задаче не применялась */
  envDir?: string
  /** полная история корневых сессий задачи (append при каждом спавне
   *  дев/ревью/тест, включая рестарты) — уходит в done-сигнал дримеру:
   *  BFS от полной истории накрывает осиротевшие сессии прошлых поколений */
  sessionHistory?: string[]
  /** URL смерженного PR (gh-проба по PR-ячейке; merge необратим — пишется
   *  один раз). Действителен, пока совпадает с ячейкой или ячейка пуста
   *  (позитив фоллбек-пробы по ветке): смена PR на заполненной клетке
   *  (stacked, ремедиация) инвалидирует кэш без перезаписи — guard на чтении */
  mergedPrUrl?: string
  /** последний доставленный промпт в дев-сессию (ms): спавн, comment-
   *  /bodyEdit-wakeup, return-to-dev. Часы тишины сталла считают от него —
   *  свежий будильник сбрасывает застой (инцидент 09.11: wake + kick одним
   *  тиком). undefined — старый state или промптов ещё не было */
  lastDevPromptAt?: number
  /** полоса подряд-unknown merged-пробы сталл-ветки (предел деградации —
   *  PR_PROBE_UNKNOWN_MAX); undefined/0 — последняя проба удалась */
  prProbeUnknownStreak?: number
  /** хэш норм-формы контента тела вне секции «Комменты» — bodyEdit-гейт:
   *  lastmod двигают UI-пересохранения и собственные записи агента, хэш —
   *  только контентные дельты (дефекты №3/№5) */
  bodyHash?: string
  /** последний доставленный будильник: идентичный ключ в коротком окне не
   *  ставится в сессию повторно (дефект №5.2, дубль-доставка) */
  lastWakeup?: WakeupStamp
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
export const TESTING_STATUS = "Тестирование"
/** Юзерский статус после pass тестировщика: мяч у владельца на мерж/приёмку. */
export const USER_REVIEW_STATUS = "Review"

export type ReviewKind = "plan" | "result"

/** Ревью-статус → вид ревью; null — обычный статус (мяч у девелопера/юзера). */
export function reviewKindOf(status: string): ReviewKind | null {
  if (status === REVIEW_PLAN_STATUS) return "plan"
  if (status === REVIEW_RESULT_STATUS) return "result"
  return null
}

/** Кому фабрика адресует агентскую сессию в этом статусе: ревью-статусы →
 *  ревьювер («review»), «Тестирование» → тестировщик («test»), остальное →
 *  девелопер («dev»). Единственная точка выбора адресата для self-эхо-среза,
 *  bodyEdit-будки и гварда тишины. */
export type SessionSlot = "review" | "test" | "dev"

export function sessionSlotOf(status: string): SessionSlot {
  if (reviewKindOf(status) !== null) return "review"
  return status === TESTING_STATUS ? "test" : "dev"
}

/** Юзерский коммент: не наш (🤖), не служебный ([factory]).
 *  Маркеры проверяются ПОСЛЕ снятия CLI-обёртки «AGENTS (ts): » — CLI выносит
 *  автора вперёд, без среза фабричные notify проходят как юзерские (дефект №2). */
export function isUserComment(c: TaskComment, botUserId?: string): boolean {
  // точный фильтр: комменты бот-аккаунта — не юзер (независимо от текста)
  if (botUserId !== undefined && c.userId !== undefined && c.userId === botUserId) {
    return false
  }
  const body = stripAgentHead(c.text.trimStart())
  if (body.startsWith(AGENT_MARK)) return false
  if (body.startsWith("[factory]")) return false
  return body.trim() !== ""
}

// ——— self-эхо wakeup-контура: маркеры, время, сесс-скопед срез (20261106032707) ———

/** Автор агентских комментов CLI (sy_dispatch): обёртка «- **AGENTS** (ts): …».
 *  Автор-скопед фильтр по этой строке — ловушка (вердики ревьювера и триаж
 *  приходят тем же маркером): здесь только признак «мог быть от сессии». */
export const AGENT_COMMENT_USER = "AGENTS"

const THREAD_REPLY_PREFIX = /^\(тред-ответ на [^)]*\)\s*/
const AGENT_HEAD = /^AGENTS \(([^)]*)\): /

/** Срезает тред-префикс и голову «AGENTS (ts): » из текста коммента. */
export function stripAgentHead(text: string): string {
  return text.replace(THREAD_REPLY_PREFIX, "").replace(AGENT_HEAD, "")
}

/** ms из головы «(2026-11-05 19:43)» коммента. CLI пишет toISOString (UTC,
 *  без зоны) — парс строго как UTC: локальная зона рантайма (демон в JST)
 *  занизила бы время на 9ч и перевернула own-срез в потерю чужих комментов. */
export function parseAgentHeadTs(text: string): number | null {
  const m = text.replace(THREAD_REPLY_PREFIX, "").match(AGENT_HEAD)
  if (m === null) return null
  const p = m[1].trim().split(/[-: ]+/).map(Number)
  if (p.length < 5 || p.some((x) => Number.isNaN(x))) return null
  // диапазоны: Date.UTC молча переливает («2026-13-45 99:99», «2026-02-30») в
  // валидные ms — битый ts не должен притворяться живым временем own-среза
  const [, , , , , second] = p
  const [year, month, day, hour, minute] = p
  if (month < 1 || month > 12 || hour > 23 || minute > 59 || (second !== undefined && second > 59)) return null
  const ms = Date.UTC(year, month - 1, day, hour, minute, second ?? 0)
  const d = new Date(ms)
  if (d.getUTCMonth() !== month - 1 || d.getUTCDate() !== day) return null
  return ms
}

export interface FreshSplit {
  /** комменты, будящие адресата (юзерские и чужие агентские) */
  foreign: TaskComment[]
  /** срезанные собственные комменты сессии */
  ownCount: number
}

/** Сесс-скопед срез self-эха (дефект №1): коммент — «свой» для сессии-адресата,
 *  если он агентский (userId AGENTS), время в нём живое и не позже последнего
 *  сообщения сессии (CLI-вызов идёт из хода, ход после коммита двигает
 *  активность). Чужие AGENTS (вердикт ревьювера, триаж) и юзерские будят.
 *  activityAt null — активность неизвестна: без среза (лишняя эхо-будка вместо
 *  потерянного коммента). ts=0 (легаси) не срезаем — неизвестное время не
 *  доказательство «своего». */
export function freshForeignComments(fresh: TaskComment[], agentActivityAt: number | null): FreshSplit {
  if (agentActivityAt === null) return { foreign: fresh, ownCount: 0 }
  let ownCount = 0
  const foreign = fresh.filter((c) => {
    const own = c.userId === AGENT_COMMENT_USER && c.createdAt > 0 && c.createdAt <= agentActivityAt
    if (own) ownCount++
    return !own
  })
  return { foreign, ownCount }
}

/** Легаси-переход после деплоя (ревью раунд-2): треки держат lastCommentId в
 *  старом непаддированном формате при lastCommentAt = 0 (createdAt комментов
 *  не жил до оживления ts). Без этой проверки фоллбек newUserComments по
 *  времени отдал бы ВСЕ чужие AGENTS-комменты одной волной будок. Идемпотентно:
 *  re-baseline двигает at, условие больше не срабатывает. Свежий спавн
 *  (id = "") и несматченный id при живом времени (штатный фоллбек) не сюда. */
export function legacyRebaselineNeeded(lastSeen: { at: number; id: string }, userComments: TaskComment[]): boolean {
  if (lastSeen.id === "" || lastSeen.at !== 0) return false
  return !userComments.some((c) => c.id === lastSeen.id)
}

/** Последний коммент по (createdAt, id) — точка спавн-границ и re-baseline. */
export function latestComment(comments: TaskComment[]): TaskComment | null {
  if (comments.length === 0) return null
  return comments.reduce((m, c) =>
    c.createdAt > m.createdAt || (c.createdAt === m.createdAt && c.id.localeCompare(m.id) > 0) ? c : m)
}

// ——— bodyEdit: контент-хэш тела вне «Комменты» (дефекты №3/№5) ———

// весь класс Format-символов (Cf): ZWSP/ZWNJ/ZWJ/WORD JOINER/BOM — рендер-
// артефакты SiYuan-экспорта (инвентарь опыта «wakeup по собственному комменту»)
const ZERO_WIDTH = /\p{Cf}/gu

/** Контент тела вне коммент-записей: (а) секция «## Комменты» (от неё до
 *  следующего ##) — историческая конвенция; (б) хвост после последней ## —
 *  CLI аппендит комменты appendBlock'ом в конец дока, в телах с секциями
 *  после «## Комменты» свежие записи живут хвостом (тела вердиктов уходят
 *  с блоками). Коммент-аппенды иммунны к хэшу — comment-контур не дублируется
 *  bodyEdit-будкой (дефект №4). Контент ДО первого хвостового пункта
 *  (абзацы отчёта и т.п.) сохраняется — его правки видны хэшу. */
export function bodyOutsideComments(md: string): string {
  const out: string[] = []
  let inComments = false
  for (const line of md.split("\n")) {
    if (/^## Комменты\s*$/.test(line)) {
      inComments = true
      continue
    }
    if (inComments && /^## /.test(line)) inComments = false
    if (!inComments) out.push(line)
  }
  let lastHead = -1
  for (let i = out.length - 1; i >= 0; i--) {
    if (/^## /.test(out[i])) {
      lastHead = i
      break
    }
  }
  for (let i = lastHead + 1; i < out.length; i++) {
    if (/^- \*\*[^*]+\*\* \(\d{4}-\d{2}-\d{2}[^)]*\): /.test(out[i])) {
      out.length = i
      break
    }
  }
  return out.join("\n")
}

/** Норм-форма markdown тела: frontmatter срезается (lastmod двигает каждое
 *  UI-пересохранение — дефект №5), format-символы SiYuan, \r\n, trailing,
 *  схлопывание пустых строк, канонизация таблиц (пэддинг/выравнивание ячеек
 *  перестраивается при UI-save — инвентарь round-trip артефактов) и пробелов
 *  после inline-bold. Правки «только форматирование» не будят; содержательные
 *  (слова, секции, порядок) — будят. */
export function normalizeBodyMd(md: string): string {
  return md
    .replace(/^---\n[\s\S]*?\n---\n/, "")
    .replace(/\r\n?/g, "\n")
    .replace(ZERO_WIDTH, "")
    .split("\n")
    .map((line) => line.startsWith("|") ? canonicalizeTableRow(line) : line.replace(/[ \t]+$/, ""))
    .join("\n")
    .replace(/\*\* +/g, "** ")
    .replace(/\n{3,}/g, "\n\n")
    .trim()
}

/** Ячейка таблицы: trim + схлопывание внутренних пробелов; separator
 *  (дефисы/двоеточия любой длины) канонизуется — выравнивание колонок при
 *  UI-save меняет пэддинг, контент не меняется. */
function canonicalizeTableRow(line: string): string {
  return line.split("|").map((cell) => {
    const trimmed = cell.trim().replace(/ {2,}/g, " ")
    return /^[-: ]+$/.test(trimmed) && trimmed !== "" ? "---" : trimmed
  }).join("|")
}

/** Хэш норм-формы контента тела вне «Комменты». */
export function bodyContentHash(md: string): string {
  return createHash("sha256").update(normalizeBodyMd(bodyOutsideComments(md))).digest("hex")
}

/** Время правки тела из frontmatter lastmod — ядро пишет с суффиксом зоны,
 *  парсится в корректный ms (SQL updated живёт в зоне контейнера, демону не
 *  видной). null — строки нет или битая дата. */
export function bodyLastmodMs(md: string): number | null {
  const m = md.match(/^lastmod:\s*(.+)$/m)
  if (m === null) return null
  const ms = Date.parse(m[1].trim())
  return Number.isNaN(ms) ? null : ms
}

// ——— дедуп доставки будильников (дефект №5.2) ———

export interface WakeupStamp {
  key: string
  at: number
}

/** Идентичный ключ в окне — скип (дельта уже доставлена); другой ключ или
 *  окно истекло — доставка. */
export function wakeupDedup(prev: WakeupStamp | undefined, key: string, now: number, windowMs: number): "deliver" | "skip" {
  if (prev !== undefined && prev.key === key && now - prev.at < windowMs) return "skip"
  return "deliver"
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
  // lastIndexOf, не findIndex: бит-в-бит дубль (тот же контент-хэш id)
  // сматчится по последнему вхождению — будильник не зациклится на хвостовой
  // копии, а снос комментов сверху не подменяет контентные id вовсе
  const idx = users.map((c) => c.id).lastIndexOf(lastSeen.id)
  if (idx >= 0) return users.slice(idx + 1)
  return users.filter((c) => c.createdAt > lastSeen.at)
}

/** Строки Todo, которых ещё нет в состоянии. */
export function untrackedTasks(rows: TaskRef[], state: FactoryState): TaskRef[] {
  return rows.filter((r) => state.tasks[r.rowId] === undefined)
}

/** Гارد-наборы GC ворктри: ветки и каталоги всего, что числится в state,
 *  снос легаси-ворктри не трогает никогда. */
export function activeTaskGuards(state: FactoryState): { branches: Set<string>; dirs: Set<string> } {
  const branches = new Set<string>()
  const dirs = new Set<string>()
  for (const task of Object.values(state.tasks)) {
    if (task.branch !== "") branches.add(task.branch)
    if (task.worktree !== "") dirs.add(task.worktree)
    if (task.envDir !== undefined && task.envDir !== "") dirs.add(task.envDir)
  }
  return { branches, dirs }
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
 *  undefined — проект вне env-механики (env:false), агент действует по
 *  прежним строкам (сам создаёт ворктри/ветку и заполняет Branch-ячейку). */
export interface SpawnEnvContext {
  repoDir: string
  branch: string
  scratchDir: string
}

/** Инит-данные первого промпта сессии: юзер (персона), проект (живые доки,
 *  чекаут для env:false), задача (метаданные строки доски). Пустые/null
 *  источники — честные строки в блоке; спавн от полноты инита не зависит. */
export interface SpawnInit {
  /** markdown персоны владельца; null — дока нет/не резолвится */
  person: string | null
  /** слаги живых доков проекта для uwuwu-cli read («<proj>/README») */
  projectDocs: string[]
  /** локальный чекаут env:false-проекта; env-задачам не нужен — пути в env-строке промпта */
  localPath?: string
  /** метаданные задачи с доски */
  task: { title: string; status: string; type: string; branch: string; pr: string }
}

const dash = (value: string): string => (value.trim() === "" ? "—" : value.trim())

/** Идентификация задачи для стартовых промптов: одна шапка на все ветки
 *  promptForStart (спавну нужен repo, конфликт-ветке — pr). */
export interface PromptTarget {
  rowId: string
  project: string
  repo: string
  /** PR строки; конфликт-kickoff ссылается на него */
  pr: string
}

/** Общий хвост параметров стартовых промптов: окружение задачи и инит-блок
 *  первого промпта. undefined-поля — легитимные режимы (env:false, без инита). */
export interface PromptContext {
  env?: SpawnEnvContext
  init?: SpawnInit
  /** корень env-механики демона (ENV_ROOT): env:false-ветка spawn-промпта
   *  называет агенту канон-путь ворктри — тот, что resolvePromptWorktree
   *  потом резолвит для tester/ревьювера */
  envRoot?: string
  /** git-ref базы env-ворктри и master-строк текста; дефолт DEFAULT_BASE */
  base?: string
}
/** YAML frontmatter из начала markdown (--- … ---) — паритет CLI
 *  markdown.rs strip_frontmatter: ядро в экспорте ведёт live-атрибуты дока. */
export function stripFrontmatter(md: string): string {
  if (!md.startsWith("---\n")) return md
  const rest = md.slice("---\n".length)
  const sep = rest.indexOf("\n---")
  if (sep === -1) return md
  return rest.slice(sep + "\n---".length).replace(/^\n+/, "")
}

/** Обёртка kernel-экспорта (frontmatter + строка `# <тайтл>`) — долой,
 *  паритет CLI strip_export_wrapper: в спавн-промпт идёт чистое тело. */
export function stripExportWrapper(md: string, docTitle: string): string {
  const body = stripFrontmatter(md).replace(/^\s+/, "")
  const titleHeading = `# ${docTitle}`
  return body.startsWith(titleHeading)
    ? body.slice(titleHeading.length).replace(/^\n+/, "")
    : body
}

/** Блок инит-данных первого промпта: ориентация без стартового поиска
 *  (person show / доки проекта / task get ради «где я»). Компактный:
 *  суть здесь, глубокое чтение — по ссылке/команде. */
export function spawnInitBlock(init: SpawnInit, rowId: string, project: string): string {
  const taskLine =
    `Задача: «${init.task.title}», статус ${dash(init.task.status)},` +
    ` тип ${dash(init.task.type)},` +
    ` ветка ${dash(init.task.branch)}, PR ${dash(init.task.pr)}.`
  const bodyLine = `Тело и треды — uwuwu-cli task get ${rowId} --project ${project} (содержание работы, не ориентация).`
  const projectParts = [
    ...(init.localPath !== undefined ? [`чекаут ${init.localPath} (env:false — ворктри/ветка сам)`] : []),
    init.projectDocs.length > 0
      ? `живые доки — uwuwu-cli read ${init.projectDocs.map((s) => `"${s}"`).join(", ")}`
      : "канон-доки (README/Домены/Стек/Деплоймент) не нашлись — глубокое чтение по месту",
  ]
  const projectLine = `Проект: ${projectParts.join("; ")}.`
  const personLine = init.person === null
    ? "Персона владельца: недоступна (uwuwu-cli person show пуст/сбой) — работай без неё."
    : [
        "Персона владельца (язык/стиль/анти-паттерны):",
        ...init.person.split("\n").map((line) => `  ${line}`),
      ].join("\n")
  return [
    "Инит-данные (первичный источник ориентации; глубокое чтение — по необходимости):",
    taskLine,
    bodyLine,
    projectLine,
    personLine,
  ].join("\n")
}

/** Вопросный канал (одинаков в spawn/continue): команды канала сразу с id
 *  и проектом — спавн знает оба (инцидент 05.11: вопросный коммент без
 *  --project не приземлился, провал незаметен в длинном ходе). */
function questionChannelLines(rowId: string, project: string): string[] {
  return [
    "ВАЖНО: юзер НЕ читает эту сессию — вся коммуникация только в uwuwu:",
    `вопросы — uwuwu-cli task comment ${rowId} --project ${project},`,
    "результаты и выводы — task body; ждёшь ответа юзера →",
    `uwuwu-cli task edit ${rowId} --project ${project} --status "Needs attention"`,
    "и завершай ход. Ответ придёт комментом — тебя разбудят.",
  ]
}

/** Стартовый промпт будильника: пайп живёт в AGENTS.md (глобально),
 *  демон знает только «какая задача» и «где работать». */
export function spawnPrompt(
  rowId: string, project: string, repo: string, mode: KickoffMode = "design",
  ctx: PromptContext = {},
): string {
  const { env, init, envRoot } = ctx
  const base = baseOf(ctx)
  const stage = mode === "implement"
    ? [
        "Статус In Progress — владелец форсировал старт (fast-track): стадию Design",
        "и «Ревью плана» пропускаешь. Если в теле ещё нет плана — короткий план",
        "первой секцией тела (без план-ревью) и сразу в работу:",
      ]
    : [
        "Стадия Design: изучи задачу/репо, утряси вопросы с юзером (комменты, статус Needs attention",
        "пока ждёшь ответа). Перед планом — скан территории:",
        `uwuwu-cli task list --project ${project} на смежные задачи`,
        "+ gh pr list --state all (батч-сводки/hygiene съедают срезы планов тихо).",
        "Строковые указатели постановки (row-id/тайтлы чужих задач) перепроверь живым",
        "uwuwu-cli task get до включения в план — доска гниёт быстрее мастера.",
        "План утрясан → план в тело → статус «Ревью плана» → конец хода:",
        "ревью проводит отдельный праймари-агент фабрики, вердикт придёт wakeup'ом.",
        "После approve плана:",
      ]
  const workLine = env === undefined
    ? [
        envRoot !== undefined
          ? `создай worktree+ветку от ${base} — ворктри ${join(envRoot, sanitizeRowId(rowId), "repo")}, скретч ${join(envRoot, sanitizeRowId(rowId), "scratch")} (не /tmp); имя ветки по сути задачи, не id; заполни Branch-ячейку и реализуй.`
          : `создай worktree+ветку от ${base} (имя по сути задачи, не id), заполни Branch-ячейку и реализуй.`,
      ]
    : [
        `работай в своём окружении задачи (фабрика создала, ветка уже от ${base}):`,
        `repo=${env.repoDir}, ветка=${env.branch}, скретч=${env.scratchDir} (не /tmp).`,
        "Branch/Path-ячейки и сессии заполняет фабрика — тебе их трогать не нужно.",
      ]
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}).`,
    ...(init !== undefined ? ["", spawnInitBlock(init, rowId, project)] : []),
    "",
    `Работай по AGENTS.md, раздел «Работа над задачей»: uwuwu-cli task get ${rowId} --project ${project} — тело, тип, комменты.`,
    ...stage,
    ...workLine,
    "Код + self-check + PR (gh pr create, URL в PR-ячейку) + отчёт в тело → статус",
    "«Ревью результата» → конец хода. Вернулся wakeup'ом с вердиктом: request_changes →",
    "устрани замечания и снова ревью-статус. Ревьювера сам не вызываешь.",
    "Без пустого шума в комментах («взялся», «отчёт») — коммент только вопрос юзеру или PR-ссылка.",
    "",
    ...questionChannelLines(rowId, project),
  ].join("\n")
}

/** Рестарт-промпт (сессия девелопера умерла/карантин → fresh spawn).
 *  Стадию НЕ предписываем: fast-track в In Progress продолжит реализацию,
 *  Design-задача — утряску; фаза живёт в теле задачи и текущем статусе. */
export function continuePrompt(
  rowId: string, project: string, repo: string, ctx: PromptContext = {},
): string {
  const { env, init } = ctx
  return [
    `[factory] Задача ${rowId} (проект ${project}, репо ${repo}): сессия разработки перезапущена.`,
    ...(init !== undefined ? ["", spawnInitBlock(init, rowId, project)] : []),
    "",
    "Работай по AGENTS.md, раздел «Работа над задачей».",
    `Перечитай uwuwu-cli task get ${rowId} --project ${project} —`,
    "тело, комменты и свой прошлый прогресс. Стадию определи по текущему статусу и телу,",
    "продолжай с места, где остановился (если работа уже идёт — не начинай её заново).",
    ...(env !== undefined
      ? [
          `Окружение задачи уже существует — своё не создавай: repo=${env.repoDir}, ветка=${env.branch},`,
          `скретч=${env.scratchDir} (не /tmp). Незакоммиченное — там же, в repo.`,
        ]
      : []),
    "",
    ...questionChannelLines(rowId, project),
  ].join("\n")
}

/** Конфликт-kickoff: PR ветки задачи конфликтует с master (владелец флипнул
 *  статус в In Progress — «поправь», или ревью вернуло задачу при конфликте).
 *  env называет существующее окружение задачи — агент не плодит новое.
 *  init — только на свежесозданной сессии (через promptForStart/startTask);
 *  живая сессия инит при спавне уже получила — будильник без init (дельта). */
export function conflictFixPrompt(
  rowId: string, pr: string, project: string, ctx: PromptContext = {},
): string {
  const { env, init } = ctx
  const base = baseOf(ctx)
  return [
    `[factory] Задача ${rowId} (проект ${project}): PR ветки конфликтует с ${base} (${pr}).`,
    ...(init !== undefined ? ["", spawnInitBlock(init, rowId, project)] : []),
    "",
    ...(env !== undefined
      ? [
          `Окружение задачи уже существует — своё не создавай: repo=${env.repoDir}, ветка=${env.branch},`,
          `скретч=${env.scratchDir} (не /tmp). Незакоммиченное — там же, в repo.`,
          "",
        ]
      : []),
    `Почини конфликт: git fetch origin, обнови ветку от ${base} (rebase/merge — как принято в репо),`,
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
 *  сквозной для всех веток — рестарт env-задачи называет окружение;
 *  init (инит-блок первого промпта) — сквозной: все три вида старта
 *  создают свежую сессию, которой нужен полный инит. */
export function promptForStart(start: StartKind, target: PromptTarget, ctx: PromptContext = {}): string {
  if (start === "continue") return continuePrompt(target.rowId, target.project, target.repo, ctx)
  if (start === "conflict-fix") return conflictFixPrompt(target.rowId, target.pr, target.project, ctx)
  return spawnPrompt(target.rowId, target.project, target.repo, start, ctx)
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

/** Будильник: тело задачи изменено контентно и не из хода адресата. Формулировка
 *  нейтральная (дополнение №3): до агента доходят только не-own дельты, но
 *  «чья правка» проверяется по телу — не дисконтировать реальную правку
 *  владельца как своё эхо. */
export function bodyEditPrompt(rowId: string): string {
  return [
    `[factory] Тело задачи ${rowId} изменено (не коммент).`,
    "",
    "Перечитай тело (uwuwu-cli task get), определи чью это правку и учти её.",
    "Хинт: единый updated у всех блоков = механическая перезапись, точечные updated = ручная правка.",
    "Если это собственная запись тебя или пайпа — эхо: тихий конец хода, без коммента и без смены статуса.",
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
  /** тип задачи из колонки «Тип»: ревьювер сверяет канон типа (Bug→TDD, Spike→без кода) */
  type?: string
  /** git-ref базы диффа и master-строк текста; дефолт DEFAULT_BASE */
  base?: string
}

/** Стартовый промпт ревьювера: фабрика спавнит сессию на ревью-статусе.
 *  agentName — праймари-агент держателя (план → plan-reviewer, результат →
 *  code-quality-reviewer): методичка адресуется по имени. */
export function reviewSpawnPrompt(
  rowId: string, project: string, kind: ReviewKind, ctx: ReviewContext, agentName: string,
): string {
  const base = baseOf(ctx)
  const lines = [
    `[factory] Задача ${rowId} (проект ${project}): ревью ${kind === "plan" ? "плана" : "результата"}.`,
    "",
    `Ты — праймари-агент ${agentName}, фабрика назначила тебя судьёй этой задачи`,
    "(1 задача = 1 твоя сессия; повторные раунды правок придут сюда же wakeup'ом).",
    "Работай по секции «Ревью задач фабрики uwuwu» своей методики:",
    "",
    `1. uwuwu-cli task get ${rowId} --project ${project} — тело задачи и треды комментов.`,
    "   Если в тредах есть прошлые раунды ревью этой задачи (твоя сессия пересоздана) —",
    "   это продолжение: проверь устранение прошлых замечаний, не начинай с нуля.",
    kind === "plan"
      ? "2. Предмет — план из тела задачи; кодовый diff не нужен (его ревью — предмет code-quality-reviewer)."
      : "2. Предмет — все изменения ветки задачи против её базы (источник branch):",
    ...(kind === "result"
      ? [`   ${base} + untracked; PR (gh pr diff/view, CI) — дополнительные артефакты.`,]
      : []),
    ...(ctx.worktree !== ""
      ? [`   Ворктри задачи: ${ctx.worktree}`]
      : kind === "result"
        ? [`   Ворктри задачи не найден — ревью по diff ${base}…${ctx.branch || "<ветка из Branch-ячейки>"} / PR`,
           `   (git-команды из каталога сессии; ворктри не обязателен для диффа).`]
        : []),
    ...(ctx.branch !== "" ? [`   Ветка: ${ctx.branch}`] : []),
    ...(ctx.pr !== "" ? [`   PR: ${ctx.pr}`] : []),
    ...(ctx.type !== undefined && ctx.type !== "" ? [`   Тип задачи: ${ctx.type}`] : []),
    "3. Отчёт — комментом в задачу (uwuwu-cli task comment), формат по своей методике,",
    kind === "plan"
      ? "   вердикт-строка «Статус: approve|request_changes» обязательна."
      : [
          "   вердикт-строка «Статус: approve|approve с мелкими замечаниями|request_changes» обязательна",
          "   (approve с мелкими замечаниями — все находки Low, методика в твоём промпте).",
        ].join("\n"),
    "4. Статус после вердикта (uwuwu-cli task edit):",
    kind === "plan"
      ? "   approve → In Progress · request_changes → Design"
      : [
          "   approve (чистый или дельта-апрув) → Тестирование",
          "   approve с мелкими замечаниями (все находки Low) → In Progress —",
          "   следующее ревью с тем же ревьювером (дельта), после дельта-апрува → Тестирование",
          "   request_changes → In Progress",
        ].join("\n"),
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
    "статус по вердикту " +
      (kind === "plan"
        ? "(approve → In Progress · request_changes → Design)."
        : [
            "(approve, чистый или дельта-апрув → Тестирование · approve с мелкими замечаниями (все находки Low) → In Progress ·",
            "request_changes → In Progress).",
          ].join("\n")),
    ...(kind === "result"
      ? [
          "Прошлый раунд был «approve с мелкими замечаниями» (свой прошлый отчёт в тредах)?",
          "Тогда это дельта-раунд: проверь только устранение тех мелочей, всё заново не пересматривай;",
          "новые High/Common — обычный request_changes. Второй раз подряд approve-with-notes не выдавай:",
          "те же мелочи не устранены → request_changes («мелочи повторно не устранены»).",
        ]
      : []),
    ...(ctx.pr !== "" ? [`PR: ${ctx.pr}`] : []),
    ...(ctx.type !== undefined && ctx.type !== "" ? [`Тип задачи: ${ctx.type}`] : []),
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

// ——— промпты тестировщика (праймари-агент на «Тестировании») ———

/** Контекст агентской стадии задачи — тот же шейп, что у ревью. */
export type TestContext = ReviewContext

/** Стартовый промпт тестировщика: фабрика спавнит сессию на «Тестировании». */
export function testingSpawnPrompt(rowId: string, project: string, ctx: TestContext): string {
  const base = baseOf(ctx)
  const lines = [
    `[factory] Задача ${rowId} (проект ${project}): стадия тестирования.`,
    "",
    "Ты — праймари-агент tester, фабрика назначила тебя тестировщиком этой задачи",
    "(1 задача = 1 твоя сессия; повторные входы придут сюда же wakeup'ом).",
    "Работай по секции «Тестирование задач фабрики uwuwu» своей методички:",
    "",
    `1. uwuwu-cli task get ${rowId} --project ${project} — тело задачи (постановка, план/решение,`,
    "   юзкейсы и «Проверка», отчёт инженера) и треды комментов (прошлые твои раунды, баг-репорты).",
    `2. Изменения: ветка против ${base} в ворктри задачи; PR (gh pr view/diff) —`,
    ...(ctx.worktree !== ""
      ? [`   Ворктри задачи: ${ctx.worktree}`]
      : [`   Ворктри задачи не найден — работай по diff ${base}…${ctx.branch || "<ветка из Branch-ячейки>"} / PR`,
         `   (git-команды из каталога сессии; ворктри не обязателен).`]),
    ...(ctx.branch !== "" ? [`   Ветка: ${ctx.branch}`] : []),
    ...(ctx.pr !== "" ? [`   PR: ${ctx.pr}`] : []),
    ...(ctx.type !== undefined && ctx.type !== "" ? [`   Тип задачи: ${ctx.type}`] : []),
    "3. Отчёт — комментом в задачу (uwuwu-cli task comment): главный сценарий (что запускали/",
    "   тыкали — команды, клики, скрины) и что сверх него; покрытие по слоям (главный путь:",
    "   юнит/граница/e2e, баг-пины); прочие проверки. Вердикт-строка",
    "   «Статус: pass|fail» обязательна.",
    "4. Статус после вердикта (uwuwu-cli task edit):",
    "   pass → Review · fail (баг-репорт с репро-шагами) → In Progress.",
    "5. Тестовые коммиты — пуш до вердикта (чистое состояние ворктри); прод-код не трогай:",
    "   проблема в прод-коде = баг → fail.",
    "6. Вопросы юзеру — комментом без смены статуса.",
  ]
  return lines.join("\n")
}

/** Будильник тестировщика: баг устранён, задача снова на «Тестировании». */
export function testingRerunPrompt(rowId: string, ctx: TestContext): string {
  return [
    `[factory] Задача ${rowId}: девелопер устранил баг — статус снова «${TESTING_STATUS}».`,
    "",
    "Дотестируй в этой же сессии: перечитай задачу (uwuwu-cli task get + треды),",
    "проверь устранение своего баг-репорта (прошлый отчёт в тредах), всё заново",
    "не пересматривай. Отчёт комментом, статус по вердикту",
    "(pass → Review · fail → In Progress). Новое покрытие — по методичке.",
    ...(ctx.pr !== "" ? [`PR: ${ctx.pr}`] : []),
    ...(ctx.worktree !== "" ? [`Ворктри: ${ctx.worktree}`] : []),
    ...(ctx.type !== undefined && ctx.type !== "" ? [`Тип задачи: ${ctx.type}`] : []),
  ].join("\n")
}

/** Будильник тестировщика: свежие комменты юзера в статусе «Тестирование». */
export function testingCommentsPrompt(rowId: string, comments: TaskComment[]): string {
  const quoted = comments.map((c) => c.text.trim()).join("\n---\n")
  return [
    `[factory] В задаче ${rowId} (стадия тестирования) новые комментарии от пользователя:`,
    "",
    quoted,
    "",
    "Ответь/учти по своему протоколу тестирования: ответ юзеру — uwuwu-cli task comment.",
  ].join("\n")
}

/** Будильник девелоперу: задача вернулась к разработке (вердикт ревьювера
 *  или тестировщика, либо ручной перевод владельца в Design). */
export function reviewReturnPrompt(rowId: string): string {
  return [
    `[factory] Задача ${rowId} вернулась к разработке — статус сменил ревьювер или тестировщик (вердикт) либо владелец.`,
    "",
    "Перечитай uwuwu-cli task get + треды и действуй по пайпу:",
    "- есть замечания ревьювера → устрани, запушь, снова поставь ревью-статус;",
    "- план approved → реализуй;",
    "- результат: approve с мелкими замечаниями → дофикси мелочи из отчёта ревьювера,",
    "  запушь и сразу «Ревью результата» — та же ревью-сессия делает дельта-проверку",
    "  (следующее ревью с тем же ревьювером; после дельта-апрува задача уйдёт на «Тестирование»);",
    "- результат approved (чистый) → задача ушла на «Тестирование», от тебя ничего не требуется;",
    "- баг-репорт тестировщика (возврат из «Тестирования») → устрани баг, прогони self-check,",
    "  запушь и поставь «Ревью результата» — код-ревью обязателен перед возвратом тестировщику;",
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
  /** вид ревью активной ревью-сессии (undefined — нет сессии или legacy-держатель);
   *  расхождение с видом статуса = спавн нового держателя, не дельта */
  reviewKind?: ReviewKind
  /** жива ли сессия тестировщика (null — liveness неизвестна: тик пропускаем решением noop) */
  testerAlive: boolean | null
  /** свежие комменты юзера */
  freshComments: TaskComment[]
}

export type RouteAction =
  | { t: "spawn-reviewer"; kind: ReviewKind }
  | { t: "rerun-reviewer"; kind: ReviewKind }
  | { t: "comments-to-reviewer" }
  | { t: "spawn-tester" }
  | { t: "rerun-tester" }
  | { t: "comments-to-tester" }
  | { t: "return-to-dev" }
  | { t: "noop" }

/** Решение по задаче на один тик. Приоритет порядка: спавн ревьювера →
 *  доревью (вход/смена вида ревью) → комменты юзера ревьюверу → стадия
 *  тестирования (спавн/дотест/комменты тестировщику) → возврат девелоперу
 *  (из ревью-статуса или «Тестирования» по вердикту, ручной перевод в Design
 *  или fast-track флип владельца в In Progress — «вмешайся и почини») → noop. */
export function routeTask(f: RouteFacts): RouteAction {
  const kind = reviewKindOf(f.status)
  if (kind !== null) {
    // Живая сессия другого вида (или legacy без вида) — спавн нового держателя:
    // вид помнит state, не статусная история (штатный план→результат идёт через
    // In Progress, lastStatus виды не различает).
    // null (liveness неизвестна, транзиентный сбой REST) — глухой noop: не плодим
    // спавны и не шлём дельту сессии неизвестного держателя — следующий тик перепроверит.
    if (f.reviewerAlive === null) return { t: "noop" }
    if (f.reviewerAlive === false || f.reviewKind !== kind) return { t: "spawn-reviewer", kind }
    if (reviewKindOf(f.lastStatus) !== kind) return { t: "rerun-reviewer", kind }
    if (f.freshComments.length > 0) return { t: "comments-to-reviewer" }
    return { t: "noop" }
  }
  if (f.status === TESTING_STATUS) {
    if (f.testerAlive === false) return { t: "spawn-tester" }
    if (f.lastStatus !== TESTING_STATUS) return { t: "rerun-tester" }
    if (f.freshComments.length > 0) return { t: "comments-to-tester" }
    return { t: "noop" }
  }
  // возврат мяча девелоперу: из ревью-статуса по вердикту ревьювера или из
  // «Тестирования» по fail-вердикту тестировщика…
  if ((reviewKindOf(f.lastStatus) !== null || f.lastStatus === TESTING_STATUS) &&
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

/** Чистый парсер UWUWU_FACTORY_STALL_MIN: undefined/пусто → 360 (6 ч,
 *  трейд владельца 08.11: глубокие фоновые прогоны — сборки/e2e/глубокое
 *  тестирование — легитимно молчат часами; ложный сталл 07.11 сработал на
 *  31-й минуте при ~90 минутах реальной работы); конечное число > 0 → оно;
 *  иначе (NaN, 0, отрицательное, мусор) → дефолт 360. 0 исключён — сигнал
 *  срабатывал бы на каждой паузе между сообщениями (анти-спам не спасёт),
 *  NaN исключён — мёртвая фича. */
export function stallMinutesFromEnv(v: string | undefined): number {
  if (v === undefined || v.trim() === "") return 360
  const n = Number(v)
  return Number.isFinite(n) && n > 0 ? n : 360
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

/** Единые часы тишины сталла: точка отсчёта — последний сигнал сессии
 *  (активность последнего сообщения или доставленный промпт). Свежий
 *  будильник сбрасывает часы: зависший после wake ход ловится тем же
 *  порогом, но от честной точки отсчёта — «wake + kick» одним тиком
 *  исключён (инцидент 09.11: fast-track-wake и сталл-сигнал из одной
 *  геометрии «тишина от активности» — два противоречащих сигнала).
 *  Потребители — stallVerdict и pre-чек ленивых проб в тике: одна
 *  арифметика на обоих. */
export interface StallClockFacts {
  /** активность последнего сообщения сессии; null — сообщений нет */
  lastActivityAt: number | null
  /** последний доставленный промпт в сессию (ms); null — не записан */
  lastDevPromptAt: number | null
}

export interface StallClock {
  /** тишина от точки отсчёта дольше порога */
  stalled: boolean
  /** тишина в минутах от точки отсчёта; null — сигналов не было вовсе */
  quietMin: number | null
  /** будка свежее последней активности — детекция ждёт ответ на неё */
  fromWake: boolean
}

export function stallClock(f: StallClockFacts, stallMs: number, now: number): StallClock {
  const lastSignalAt = Math.max(f.lastActivityAt ?? 0, f.lastDevPromptAt ?? 0)
  if (lastSignalAt === 0) return { stalled: true, quietMin: null, fromWake: false }
  return {
    stalled: now - lastSignalAt > stallMs,
    quietMin: Math.max(1, Math.round((now - lastSignalAt) / 60_000)),
    fromWake: (f.lastDevPromptAt ?? 0) > (f.lastActivityAt ?? 0),
  }
}

export interface StallFacts {
  /** статус строки сейчас */
  status: string
  /** жива ли сессия девелопера (null — liveness неизвестна: пропуск тика) */
  sessionAlive: boolean | null
  /** активность последнего сообщения; null — сообщений нет (= застой) */
  lastActivityAt: number | null
  /** последний доставленный промпт в дев-сессию (ms); null — не записан */
  lastDevPromptAt: number | null
  /** Branch-ячейка строки (деталь диагноза в тексте сигнала) */
  branch: string
  /** ветка нашлась в ворктри по `git worktree list` */
  branchInWorktree: boolean
  /** PR строки смержен (gh-проба по URL ячейки, позитив-кэш в state):
   *  тишина сессии — штатное ожидание приёмки, сигнал не нужен */
  prMerged: boolean
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
  const clock = stallClock(
    { lastActivityAt: f.lastActivityAt, lastDevPromptAt: f.lastDevPromptAt },
    stallMs,
    f.now,
  )
  if (!clock.stalled) return { t: "none" }
  // PR смержен = работа закончена, тишина сессии — штатное ожидание приёмки
  // (мяч у владельца на Done); сигнал был бы ложным (прод-кейс 06.11:
  // финализированная задача получила stall-wakeup). none — флаг анти-спама,
  // если стоял, снимет штатная none-ветка демона («эпизод завершён»).
  if (f.prMerged) return { t: "none" }
  if (f.signaled) return { t: "silent" }
  const quietText = clock.quietMin === null ? "сообщений в сессии нет" : `тишина ${clock.quietMin} мин`
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

// ——— merged-проба сталл-ветки: fail-open с пределом деградации ———

export type PrProbeResult = "merged" | "open" | "unknown"

/** Предел подряд-unknown merged-пробы: дальше — деградация к not-merged
 *  (страховка ADR-044 возвращается, слепая зона ограничена окном тиков). */
export const PR_PROBE_UNKNOWN_MAX = 3

export interface PrProbeVerdict {
  merged: boolean
  /** unknown до предела: тик без сигнала — кик откладывается, полоса набирается */
  defer: boolean
  /** длина полосы unknown после этого тика (0 — проба удалась) */
  streakAfter: number
  /** true — полоса unknown превысила предел, кик идёт с громким логом */
  degraded: boolean
}

/** Решение по merged-пробе (инцидент 09.11: сбой gh съедался в false —
 *  ложный кик задачи со смерженным PR). unknown откладывает кик на тик
 *  (defer — сталл-вердикт в этом тике не строится); полоса дольше
 *  PR_PROBE_UNKNOWN_MAX деградирует к not-merged — прежний catch→false был
 *  злее (ложный кик), вечный unknown — новая слепая зона. */
export function prProbeVerdict(result: PrProbeResult, streakBefore: number): PrProbeVerdict {
  if (result !== "unknown") return { merged: result === "merged", defer: false, streakAfter: 0, degraded: false }
  const streakAfter = streakBefore + 1
  const degraded = streakAfter >= PR_PROBE_UNKNOWN_MAX
  return { merged: false, defer: !degraded, streakAfter, degraded }
}

// ——— адресат будки правки тела ———

export type BodyEditRole = "ревьювер" | "тестировщик" | "девелопер"

export type BodyEditRoute =
  | { t: "wake"; target: string; role: BodyEditRole }
  | { t: "swallow"; /** осознанное проглатывание (гейт статуса) — проводка обновляет базлайн */
      updateBaseline: boolean }

/** Кому адресовать будку контентной правки тела (инцидент 09.11: отчёт
 *  тестировщика в теле на Review-строке будил дев-сессию — холостая будка
 *  на каждом тест-раунде с отчётом). Review = мяч у владельца: правки туда
 *  не доставляются (swallow с обновлением базлайна в проводке). Агентская
 *  стадия — держателю сессии; без живого держателя — фоллбек на дева
 *  (прежнее поведение проводки), без дева (карантин) — ждать адресата. */
export function bodyEditRoute(f: {
  status: string
  reviewSessionID: string
  testSessionID: string
  devSessionID: string
}): BodyEditRoute {
  if (f.status === USER_REVIEW_STATUS) return { t: "swallow", updateBaseline: true }
  const toDev: BodyEditRoute = f.devSessionID === ""
    ? { t: "swallow", updateBaseline: false }
    : { t: "wake", target: f.devSessionID, role: "девелопер" }
  if (reviewKindOf(f.status) !== null) {
    return f.reviewSessionID === "" ? toDev : { t: "wake", target: f.reviewSessionID, role: "ревьювер" }
  }
  if (f.status === TESTING_STATUS) {
    return f.testSessionID === "" ? toDev : { t: "wake", target: f.testSessionID, role: "тестировщик" }
  }
  return toDev
}

// ——— гвард ревью-тишины: недоставленный вердикт ревьювера (инцидент 07.11,
//      задача 20261107142321) ———

export interface ReviewStallFacts {
  /** статус строки сейчас */
  status: string
  /** жива ли сессия держателя статуса (ревьювер/тестировщик; null —
   *  liveness неизвестна: тик пропускаем решением none; dead — routeTask
   *  сам спавнит нового держателя) */
  holderAlive: boolean | null
  /** активность последнего сообщения сессии держателя; null — сообщений нет
   *  (= застой: спавн не начал ход или потерялся) */
  lastActivityAt: number | null
  /** сигнал уже отправлен в этом эпизоде ревью-статуса (анти-спам) */
  signaled: boolean
  now: number
}

export type ReviewStallVerdict =
  | { t: "none" }
  | { t: "signal"; text: string }
  | { t: "silent" }

/** Единственная точка решения о тишине держателя ревью/тест-статуса (по
 *  образцу stallVerdict): агентская стадия при живой сессии держателя,
 *  молчащей дольше порога, и статусе, не ушедшем из стадии, = ход завершился
 *  без доставки (вердикт-коммент/статус потерялись) или агент ждёт юзера
 *  (вопрос в тредах). routeTask в этой геометрии возвращает noop бессрочно —
 *  конвейер без гарда висит тихо. Вердикт signal → эскалация юзеру (Needs
 *  attention, восстановление штатными путями: перестановка статуса будит ту
 *  же сессию доревью-промптом). Повторный сигнал в эпизоде глушится флагом
 *  (silent); сброс — дело демона: смена статуса или вернувшаяся активность. */
export function reviewStallVerdict(f: ReviewStallFacts, stallMs: number): ReviewStallVerdict {
  if (sessionSlotOf(f.status) === "dev") return { t: "none" }
  if (f.holderAlive !== true) return { t: "none" }
  const stalled = f.lastActivityAt === null || f.now - f.lastActivityAt > stallMs
  if (!stalled) return { t: "none" }
  if (f.signaled) return { t: "silent" }
  const who = f.status === TESTING_STATUS ? "тестировщика" : "ревьювера"
  const quietMin = f.lastActivityAt === null
    ? null
    : Math.max(1, Math.round((f.now - f.lastActivityAt) / 60_000))
  const quietText = quietMin === null ? "сообщений в сессии нет" : `тишина ${quietMin} мин`
  const text = [
    `[factory] Задача в статусе «${f.status}», но сессия ${who} молчит: ${quietText} при пороге ${Math.round(stallMs / 60_000)} мин,`,
    "статус из стадии не ушёл — ход завершился без доставки (вердикт-коммент/статус потерялись) или агент ждёт тебя.",
    "Проверь треды: если агент задал вопрос — ответь комментом; если вердикта нет — это потеря доставки.",
    "Затем переставь статус стадии: сессия будет разбужена дельта-промптом, перечитает задачу с тредами",
    "(увидит и твой ответ) и доставит недостающее.",
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
      "Если это заявленные агентами строки (task claim) с мёртвыми сессиями — state не тронут: очисти или перезапиши ячейку «Сессия dev», гард снимется сам. " +
      "Если волна ожидаема — подними UWUWU_FACTORY_SPAWN_LOSS_MAX / UWUWU_FACTORY_SPAWN_BURST_MAX и перезапусти демон."
    : `[factory] Спавн задержан — за один тик набралось untracked=${f.totalUntracked} при пороге ${l.burstMax} ` +
      `(сирот-спавнов=${f.orphaned}): массовый импорт задач или потеря state без свидетелей-ячеек. ` +
      "Если волна ожидаема — подними UWUWU_FACTORY_SPAWN_BURST_MAX и перезапусти демон; иначе проверь state.json."
  return { t: "hold", reason, alert, text }
}

/** Свидетель спавна в строке: сессионная ячейка непуста. Поля опциональны —
 *  undefined (TaskRef без нормализации) свидетельством НЕ является. */
export function hasSessionEvidence(r: Pick<TaskRef, "devSession" | "reviewSession" | "testSession">): boolean {
  return (r.devSession ?? "") !== "" || (r.reviewSession ?? "") !== "" || (r.testSession ?? "") !== ""
}

/** Живость сессии по REST-пробе опенкода: "unknown" — проба не удалась
 *  (транзиентный сбой), решение принимает вызывающий. */
export type SessionLiveness = "alive" | "dead" | "unknown"

/** Вердикт тика по untracked строке: спавнить ли её в дев-волне и считать ли
 *  сиротой-спавном (корм гарда каскада). Единственная точка решения (по
 *  образцу routeTask) — матрица плана задачи 20261105174319:
 *  - дев-ячейка пуста → обычный путь (дев-волна решает по статусу), сирота
 *    при ревью- или тест-свидетельстве (карантинный спавн ревьювера/тестировщика
 *    пишет только свою ячейку — защита инцидента 06.11 сохраняется);
 *  - живая дев-заявка (task claim или фабричный спавн) → внешняя работа
 *    идёт: не спавнить и не сирота;
 *  - liveness-проба не удалась → трактуем как живую (прецедент routeTask):
 *    транзиентный сбой REST не даёт ни спавна, ни orphan-счёта, следующий
 *    тик перепроверит;
 *  - мёртвая дев-заявка → сирота как прежде И остаётся в дев-волне: семантика
 *    сирот не менялась — при allow ре-спавн компенсирует (это путь автоком-
 *  пенсации lossMax-допуска: 1-2 сироты спавнятся, ≥3 — HOLD). Отличить
 *  «мёртвый claim» от «потери state» по одной строке невозможно, поэтому
 *  обе ситуации ведут себя одинаково: сирота в гарде + спавн при allow. */
export interface UntrackedRowDecision {
  /** спавнить в дев-волне этого тика (финально фильтруется статусом) */
  devWave: boolean
  /** считать сиротой-спавном в подсчёте гарда */
  orphan: boolean
}

export function untrackedRowDecision(
  r: Pick<TaskRef, "devSession" | "reviewSession" | "testSession">,
  liveness: SessionLiveness,
): UntrackedRowDecision {
  const devCell = r.devSession ?? ""
  if (devCell === "") {
    return { devWave: true, orphan: (r.reviewSession ?? "") !== "" || (r.testSession ?? "") !== "" }
  }
  // unknown (проба не удалась) трактуем как живую — skip, как в routeTask
  if (liveness !== "dead") return { devWave: false, orphan: false }
  return { devWave: true, orphan: true }
}

/** Адресат алерта — фолбэк-цепочка по порядку скана доски: сирота с телом →
 *  первая строка с телом (чистый burst без сирот) → null (ни одной строки
 *  с телом — алерт недоставим, терминальный лог-only случай). */
export function alertTargetOf(candidates: TaskRef[]): TaskRef | null {
  const orphanWithBody = candidates.find((r) => r.bodyDoc !== "" && hasSessionEvidence(r))
  if (orphanWithBody !== undefined) return orphanWithBody
  return candidates.find((r) => r.bodyDoc !== "") ?? null
}

/** Ячейки доски, которые заполняет фабрика. Колонки добавляет владелец;
 *  запись best-effort (нет колонки → тихий no-op ядра, работа не ломается). */
export const PATH_CELL = "Path"
export const DEV_SESSION_CELL = "Сессия dev"
export const REVIEW_SESSION_CELL = "Сессия ревью"
export const TEST_SESSION_CELL = "Сессия тест"

/** Валидация записи state.json. Легальные форматы: полный (ses_ + пути),
 *  чистый карантин (всё пусто) и гибрид (сессия умерла, пути живы — работа
 *  сохраняется до решения юзера). worktree = repoDir env-задачи; branch —
 *  производное имя ветки (пишет фабрика). */
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
  if (t.reviewStallSignaled !== undefined && typeof t.reviewStallSignaled !== "boolean") return false
  if (t.envDir !== undefined && !(typeof t.envDir === "string" && t.envDir.startsWith("/"))) return false
  if (t.sessionHistory !== undefined && !(Array.isArray(t.sessionHistory)
    && t.sessionHistory.every((s) => typeof s === "string" && s.startsWith("ses_")))) return false
  if (
    t.mergedPrUrl !== undefined &&
    !(typeof t.mergedPrUrl === "string" && t.mergedPrUrl.startsWith("http"))
  ) return false
  if (t.lastDevPromptAt !== undefined && typeof t.lastDevPromptAt !== "number") return false
  if (t.prProbeUnknownStreak !== undefined && typeof t.prProbeUnknownStreak !== "number") return false
  if (t.bodyHash !== undefined && typeof t.bodyHash !== "string") return false
  if (
    t.lastWakeup !== undefined &&
    !(
      typeof t.lastWakeup === "object" && t.lastWakeup !== null &&
      typeof (t.lastWakeup as WakeupStamp).key === "string" &&
      typeof (t.lastWakeup as WakeupStamp).at === "number"
    )
  ) return false
  if (
    t.reviewSessionID !== undefined &&
    !(t.reviewSessionID === "" || (typeof t.reviewSessionID === "string" && t.reviewSessionID.startsWith("ses_")))
  ) return false
  if (t.reviewAttempts !== undefined && typeof t.reviewAttempts !== "number") return false
  if (t.reviewKind !== undefined && t.reviewKind !== "plan" && t.reviewKind !== "result") return false
  if (
    t.testSessionID !== undefined &&
    !(t.testSessionID === "" || (typeof t.testSessionID === "string" && t.testSessionID.startsWith("ses_")))
  ) return false
  if (t.testAttempts !== undefined && typeof t.testAttempts !== "number") return false
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
  /** git-ref базы env-ворктри и текстов промптов. Репы не на master
   *  (GitLab и пр. — origin/main) указывают свой ref. Дефолт DEFAULT_BASE. */
  base?: string
  /** env-механика задачи (worktree+скретч в ~/agents-env). false — проект
   *  вне механики (cfg.local не git-репо → контракт невыполним): агент сам
   *  делает ворктри/ветку и заполняет Branch-ячейку. Дефолт true. */
  env?: boolean
}

/** База env-ворктри по умолчанию: все живые проекты фабрики — master-based. */
export const DEFAULT_BASE = "origin/master"

/** База из конфига/контекста промпта с дефолтом; пустая строка — это
 *  заданная (сломанная) база, не отсутствие. Валидность пустой — parseProjects. */
export function baseOf(source: { base?: string }): string {
  return source.base ?? DEFAULT_BASE
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
    if (c.base !== undefined && (typeof c.base !== "string" || c.base.trim() === "")) {
      throw new Error(`UWUWU_FACTORY_PROJECTS_JSON: проект «${name}» — base должна быть непустой строкой`)
    }
    out[name] = {
      board: c.board, repo: c.repo, local: c.local,
      ...(c.base !== undefined ? { base: c.base } : {}),
      ...(c.env !== undefined ? { env: c.env } : {}),
    }
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
  /** спавн/рестарт тестировщиков */
  testerSpawned: number
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
  return `проход ${project}: спавн dev=${c.devSpawned} ревью=${c.reviewSpawned} тест=${c.testerSpawned}` +
    `, будильники=${c.wakeups}, чистки=${c.cleanups}${envPart}${heldPart}, наблюдение=${c.watched}${failures}`
}
