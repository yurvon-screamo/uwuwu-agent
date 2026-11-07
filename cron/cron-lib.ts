// cron-lib.ts — чистые функции uwuwu-cron: расписания, промпты, состояние.
// Schedule: cron-выражение (5 полей), интервал (5m/2h/1d) или разовое at HH:MM.

export interface CronRow {
  rowId: string
  bodyDoc: string
  title: string
  /** Расписание из custom-schedule дока-тела; пусто для обычных задач. */
  schedule: string
  /** Клетка «Запланировано» — epoch ms (сравнение по content: formatted
   *  ядро перекрывает своей локалью); null — пусто/не задано. */
  plannedMs: number | null
  /** Клетка «Расписание» (отображение custom-schedule; "" — пусто). */
  scheduleCell: string
  /** Статус строки доски (mSelect, нижний регистр). Файр — только «вейт». */
  status: string
}

export interface TrackedCron {
  sessionID: string
  lastFiredAt: number
  firedAtIso: string
  /** Причина последнего отказа (дедуп комментов: коммент — только при смене).
   *  Сбрасывается при успешном файре. */
  lastRefusal?: string
}

export interface CronState {
  crons: Record<string, TrackedCron>
}

/** Локальное время «YYYY-MM-DD HH:MM» — метки запусков, клетки
 *  «Запланировано» и раны читает юзер; формат стабилен для сравнения
 *  reconcile демона (клетку пишет CLI тем же форматированием). */
export function formatLocal(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}

// ——— расписания ———

export type Sched =
  | { kind: "interval"; ms: number }
  | { kind: "cron"; fields: string[] }
  | { kind: "at"; hour: number; minute: number; day?: number; month?: number }

/** Разбор строки расписания; null = кривое расписание. */
export function parseSchedule(s: string): Sched | null {
  const t = s.trim()
  let m = /^(\d+)([mhd])$/.exec(t)
  if (m !== null) {
    const n = Number(m[1])
    if (n <= 0) return null
    const unit = m[2] === "m" ? 60_000 : m[2] === "h" ? 3_600_000 : 86_400_000
    return { kind: "interval", ms: n * unit }
  }
  m = /^(\d{1,2}):(\d{2})(?:\s+(\d{1,2})\.(\d{1,2}))?$/.exec(t)
  if (m !== null) {
    const hour = Number(m[1]), minute = Number(m[2])
    if (hour > 23 || minute > 59) return null
    const day = m[3] !== undefined ? Number(m[3]) : undefined
    const month = m[4] !== undefined ? Number(m[4]) : undefined
    // диапазоны даты проверяются на парсинге (паритет с CLI-валидацией):
    // «45.13» иначе разбирался бы «легально» и молча не срабатывал никогда
    if (day !== undefined && (day < 1 || day > 31)) return null
    if (month !== undefined && (month < 1 || month > 12)) return null
    return {
      kind: "at", hour, minute,
      ...(day !== undefined ? { day, month } : {}),
    }
  }
  const fields = t.split(/\s+/)
  if (fields.length !== 5) return null
  // каждое поле: запятая-список из (*|N[-M])[/step≥1] — мусор вида "abc"
  // иначе парсится «легально» и молча не срабатывает никогда
  const partRe = /^(\*|\d+(?:-\d+)?)(?:\/[1-9]\d*)?$/
  const ranges: Array<[number, number]> = [[0, 59], [0, 23], [1, 31], [1, 12], [0, 6]]
  const valid = fields.every((f, i) => {
    const range = ranges[i]
    if (range === undefined) return false
    const [min, max] = range
    return f.split(",").every(p => {
      if (!partRe.test(p) || p.endsWith("/")) return false
      const m = /^(\*|\d+)(?:-(\d+))?/.exec(p)
      if (m === null || m[1] === undefined) return false
      const lo = m[1] === "*" ? min : Number(m[1])
      const hi = m[2] !== undefined ? Number(m[2]) : lo
      return lo >= min && hi <= max && lo <= hi
    })
  })
  return valid ? { kind: "cron", fields } : null
}

/** Совпадение одного cron-поля (мин/час/день/мес/день-недели) со значением. */
export function fieldMatches(field: string, value: number, min: number, max: number): boolean {
  for (const part of field.split(",")) {
    let step = 1
    const slash = part.indexOf("/")
    let range = part
    if (slash >= 0) {
      step = Number(part.slice(slash + 1))
      if (!Number.isInteger(step) || step <= 0) continue
      range = part.slice(0, slash)
    }
    let lo = min, hi = max
    if (range !== "*") {
      const dash = range.indexOf("-")
      if (dash >= 0) {
        lo = Number(range.slice(0, dash))
        hi = Number(range.slice(dash + 1))
      } else {
        lo = hi = Number(range)
      }
      if (!Number.isInteger(lo) || !Number.isInteger(hi) || lo > hi) continue
    }
    if (step === 1 ? value >= lo && value <= hi
        : Array.from({ length: hi - lo + 1 }, (_, i) => lo + i).some(v => (v - lo) % step === 0 && v === value)) {
      return true
    }
  }
  return false
}

/** POSIX-семантика dom/dow: оба * → любое; один задан → он; оба → ИЛИ. */
function cronMatches(fields: string[], d: Date): boolean {
  const [mi, h, dom, mon, dow] = fields
  const domStar = dom === "*", dowStar = dow === "*"
  const dayMatch = domStar && dowStar ? true
    : domStar ? fieldMatches(dow, d.getDay(), 0, 6)
    : dowStar ? fieldMatches(dom, d.getDate(), 1, 31)
    : fieldMatches(dom, d.getDate(), 1, 31) || fieldMatches(dow, d.getDay(), 0, 6)
  return (
    fieldMatches(mi, d.getMinutes(), 0, 59) &&
    fieldMatches(h, d.getHours(), 0, 23) &&
    fieldMatches(mon, d.getMonth() + 1, 1, 12) &&
    dayMatch
  )
}

/** Наступило ли время срабатывания. */
export function due(sched: Sched, lastFiredAt: number, now: Date): boolean {
  switch (sched.kind) {
    case "interval":
      return lastFiredAt === 0 || now.getTime() - lastFiredAt >= sched.ms
    case "cron":
      return cronMatches(sched.fields, now) && minuteStart(now) > lastFiredAt
    case "at": {
      if (sched.day !== undefined && (now.getDate() !== sched.day || now.getMonth() + 1 !== (sched.month ?? now.getMonth() + 1))) return false
      // догон: минута наступила (в т.ч. пропущена из-за дауна демона) —
      // стреляем поздно один раз, fired-статус прикроет повтор
      const scheduled = new Date(now.getFullYear(), now.getMonth(), now.getDate(), sched.hour, sched.minute)
      return now.getTime() >= scheduled.getTime() && minuteStart(now) > lastFiredAt
    }
  }
}

/** Разовое расписание (at): после срабатывания демон снимает custom-schedule —
 *  мастер покидает крон-пул и не повторится; повторный запуск — заново задать
 *  расписание (task edit --schedule). */
export function isOneShot(sched: Sched): boolean {
  return sched.kind === "at"
}

/** Следующее планируемое срабатывание после момента `after` (минутная
 *  точность — compare с клеткой «Запланировано» доски). null — срабатываний
 *  дальше нет (разовый в прошлом; cron-выражение, не совпадающее за год —
 *  скан ограничен календарным годом, вечный молчун клетку не трогает).
 *  Семантика зеркалит due(): интервал — от последнего файра (никогда не
 *  стрелявший файрится следующим тиком, вызов передаёт now), at — момент
 *  HH:MM [DD.MM], если он впереди. */
export function nextFire(sched: Sched, after: Date): Date | null {
  switch (sched.kind) {
    case "interval": {
      return new Date(minuteStart(after) + sched.ms)
    }
    case "at": {
      // day и month приходят парой из parseSchedule (HH:MM DD.MM); без них —
      // ежедневный at: сегодняшний момент, если он ещё впереди
      const moment = new Date(
        after.getFullYear(),
        sched.month !== undefined ? sched.month - 1 : after.getMonth(),
        sched.day ?? after.getDate(),
        sched.hour,
        sched.minute,
      )
      return moment.getTime() > after.getTime() ? moment : null
    }
    case "cron": {
      const minuteMs = 60_000
      const horizon = new Date(after.getFullYear() + 1, after.getMonth(), after.getDate()).getTime()
      let t = minuteStart(after) + minuteMs
      while (t < horizon) {
        const candidate = new Date(t)
        if (cronMatches(sched.fields, candidate)) return candidate
        t += minuteMs
      }
      return null
    }
  }
}

/** Решение по свежему срезу доски прямо перед спавном: строка могла быть
 *  снята/закрыта/снята с Вейт, пока тик обрабатывал очередь. Гонка «rm ↔ файр»
 *  (живой кейс 05.11: файр по строке, снятой в ходе тика) сужается до окна
 *  render → POST. Файр — только строка в статусе «вейт» (мастер крон-задачи). */
export function fireGate(rows: CronRow[], rowId: string): "fire" | "removed" | "not-wait" {
  const row = rows.find((r) => r.rowId === rowId)
  if (row === undefined) return "removed"
  return row.status === "вейт" ? "fire" : "not-wait"
}

export function minuteStart(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes()).getTime()
}

/** Расхождение клеток мастера с расписанием и last-fire: что CLI должен
 *  дописать (пустой объект — доска честна). «Расписание» — отображение
 *  custom-schedule (сбой записи клетки после attr иначе молча старел бы),
 *  «Запланировано» — next-fire после последнего файра. Сравнение даты —
 *  по epoch ms. Чистая функция — юнит-тесты без моков; применение —
 *  reconcileMasterCells (DRY_RUN-гвард там). */
export function cellPatches(row: CronRow, sched: Sched, prevFire: number, now: Date): { schedule?: string; planned?: string } {
  const patch: { schedule?: string; planned?: string } = {}
  if (row.scheduleCell !== row.schedule) patch.schedule = row.schedule
  // гард вечного молчуна: never-fired + пустая клетка — вычислять next нечего.
  // due при lastFired=0 файрит следующим тиком (интервал — всегда, cron/at —
  // на первом совпадении), после файра reconcile посчитает next от prevFire.
  // Без гарда грамматически валидный cron без совпадений за год («0 0 30 2 *»)
  // гонял бы полный годовой скан каждым тиком вечно
  if (prevFire === 0 && row.plannedMs === null) return patch
  const next = nextFire(sched, prevFire > 0 ? new Date(prevFire) : now)
  // compare по epoch ms: formattedContent ядро перекрывает своей локалью
  // (живая проба миграции) — строкой клетки и вычисления не совпадут никогда
  const plannedMs = next !== null ? minuteStart(next) : null
  if (row.plannedMs !== plannedMs) patch.planned = next !== null ? formatLocal(next) : ""
  return patch
}

/** Валидация state-записи. */
export function isValidTracked(v: unknown): v is TrackedCron {
  if (v === null || typeof v !== "object") return false
  const t = v as Record<string, unknown>
  return (
    (t.sessionID === "" || (typeof t.sessionID === "string" && t.sessionID.startsWith("ses_"))) &&
    typeof t.lastFiredAt === "number" &&
    (t.firedAtIso === undefined || typeof t.firedAtIso === "string") &&
    (t.lastRefusal === undefined || typeof t.lastRefusal === "string")
  )
}

// ——— резолв и валидация тела ———

/** Тайтл → безопасный сегмент hpath (конвенция CLI path_safe: '/' → U+2215). */
export function pathSafe(title: string): string {
  return title.replace(/\//g, "∕")
}

/** Снятие экспорт-обвязки и невидимых символов: YAML-frontmatter, заголовок-эхо
 *  дока (exportMdContent всегда препендит `# <title>` — это метаданные, не
 *  постановка), HTML-комментарии (в т.ч. unsupported-легаси) и zero-width.
 *  Экспорт тела, состоящего после этого из пустоты, рендерится агенту в
 *  пустоту — инцидентный класс «тело из unsupported-комментариев → сессия
 *  с пустым промптом». */
export function stripNonContent(md: string): string {
  let text = md.replace(/^---\n[\s\S]*?\n---\n/, "")
  text = text.replace(/^\s*#[^\n]*\n?/, "")
  text = text.replace(/<!--[\s\S]*?-->/g, "")
  return text.replace(/[\u200B-\u200D\uFEFF]/g, "").trim()
}

/** Причина, по которой рендер тела непригоден как промпт; null = тело валидно. */
export function bodyRenderIssue(md: string): string | null {
  if (stripNonContent(md) === "") {
    return "тело рендерится в пусто/unsupported-комментарии"
  }
  return null
}

export type BodyChoice =
  | { kind: "doc"; docId: string }
  | { kind: "missing" }
  | { kind: "duplicates"; docIds: string[] }

/** Паритет CLI choose_body_doc: живая привязка → она; мёртвая/отсутствующая →
 *  fallback по заголовку: 0 доков → нет тела; 1 → он; ≥2 → дубли (ошибка). */
export function chooseBodyDoc(bound: string, boundLive: boolean, titleDocIds: string[]): BodyChoice {
  if (bound !== "" && boundLive) return { kind: "doc", docId: bound }
  if (titleDocIds.length === 0) return { kind: "missing" }
  const [only] = titleDocIds
  if (titleDocIds.length === 1 && only !== undefined) return { kind: "doc", docId: only }
  return { kind: "duplicates", docIds: titleDocIds }
}

/** Тайтл сессии из тела: первая содержательная строка после стрипа —
 *  frontmatter и unsupported-мусор в имя сессии не попадают. */
export function titleFromBody(body: string): string {
  for (const line of stripNonContent(body).split("\n")) {
    const t = line.replace(/^#+\s*/, "").replace(/^[-*]\s*/, "").trim()
    if (t !== "") return t
  }
  return ""
}

// ——— резолв тела крона (оркестрация над инжектируемым платформенным tool) ———

/** Структурный тип платформенного tool (инжект — юнит-тесты без ядра). */
export type PlatformTool = <T = Record<string, unknown>>(name: string, args: Record<string, unknown>) => Promise<T>

export type BodyResolution =
  | { kind: "ok"; docId: string; markdown: string }
  | { kind: "refuse"; reason: string }
  | { kind: "infra"; error: string }

/** Вердикт по рендеру тела: unsupported-комментарии/пустота → refuse. */
function renderVerdict(docId: string, markdown: string): BodyResolution {
  const issue = bodyRenderIssue(markdown)
  if (issue !== null) {
    return { kind: "refuse", reason: `${issue} (docId ${docId})` }
  }
  return { kind: "ok", docId, markdown }
}

/** Док-тело крона: привязка строки (id-first), мёртвая/отсутствующая →
 *  title-fallback по hpath доски (контракт CLI choose_body_doc). Сбоям ядра
 *  (infra) вердикт не выносится: refuse — только при подтверждённом
 *  отсутствии/пустоте тела, иначе слот крона сжигался бы минутными сбоями. */
export async function resolveBody(tool: PlatformTool, boardDocId: string, row: CronRow): Promise<BodyResolution> {
  try {
    if (row.bodyDoc !== "") {
      const exported = await tool<{ exists?: boolean; markdown?: string }>("export_doc_markdown", { docId: row.bodyDoc })
      if (exported.exists === true) {
        return renderVerdict(row.bodyDoc, exported.markdown ?? "")
      }
      // export глотает любые ошибки в exists:false — мёртвую привязку
      // отличаем от сбоя ядра пробой block get (FS-истина)
      const live = await tool<{ exists?: boolean }>("doc_exists", { docId: row.bodyDoc })
      if (live.exists === true) {
        return { kind: "infra", error: `export дока ${row.bodyDoc} дал exists:false при живом блоке` }
      }
    }
    if (row.title === "") {
      return { kind: "refuse", reason: "строка без тела: нет ни привязки, ни заголовка для title-fallback" }
    }
    const info = await tool<{ box?: string; hpath?: string }>("get_doc", { docId: boardDocId })
    // SQL-лаг индекса молча даёт пустые box/hpath — fallback с пустым
    // notebook «подтвердил бы отсутствие» ложно: это infra, не вердикт
    if ((info.box ?? "") === "" || (info.hpath ?? "") === "") {
      return { kind: "infra", error: `доска ${boardDocId} без box/hpath в индексе (лаг SQL?)` }
    }
    const byTitle = await tool<{ ids?: string[] }>("doc_ids_by_hpath", {
      notebook: info.box, path: `${info.hpath}/${pathSafe(row.title)}`,
    })
    const choice = chooseBodyDoc(row.bodyDoc, false, byTitle.ids ?? [])
    if (choice.kind === "duplicates") {
      return { kind: "refuse", reason: `дубли тел по заголовку (${choice.docIds.join(", ")}) — вычисти сироты в UI` }
    }
    if (choice.kind === "missing") {
      return { kind: "refuse", reason: `строка без тела: привязки нет, саб-дока «${row.title}» у доски нет` }
    }
    const exported = await tool<{ exists?: boolean; markdown?: string }>("export_doc_markdown", { docId: choice.docId })
    if (exported.exists !== true) {
      return { kind: "infra", error: `title-fallback док ${choice.docId}: export дал exists:false` }
    }
    return renderVerdict(choice.docId, exported.markdown ?? "")
  } catch (e) {
    return { kind: "infra", error: String(e) }
  }
}

// ——— промпты ———

export function sanitizeBoundary(text: string): string {
  return text.replace(/^\s*===.*$/gm, "≡")
}

/** Промпт срабатывания: тело мастера + тихий протокол рана. Ран-карточка —
 *  запись запуска (создаёт демон до спавна); коммент агента идёт на ран.
 *  Тело непусто по инварианту тика: гейт резолвит и валидирует его до
 *  спавна — пустое/unsupported-тело не доходит до промпта (refuse). */
export function firePrompt(
  master: { title: string; schedule: string },
  body: string,
  runRowId: string,
  project: string,
): string {
  return [
    `Ты — запуск крон-задачи «${master.title.replace(/[\r\n]+/g, " ")}» (расписание: ${master.schedule}).`,
    `Ран-карточка этого запуска — задача ${runRowId} на доске «Задачи ${project}»: она уже создана, работать по ней не нужно.`,
    "",
    "=== ТЕЛО ЗАДАЧИ ===",
    sanitizeBoundary(body),
    "=== КОНЕЦ ТЕЛА ===",
    "",
    "ПРАВИЛА:",
    "1. Выполни работу из тела. Это периодическая проверка/работа — работай автономно.",
    "2. Здоровый ран — тихий: никаких отчётных комментов и доков-отчётов; ран-карточка — запись запуска, её статус не меняй (закрывает владелец). Правки и записи, которые предписывает само тело задачи, — нормальны; запрещён только шум ради отчёта.",
    "3. Коммент — ТОЛЬКО исключение (падение проверки, блокер, вопрос человеку):",
    `   uwuwu-cli task comment ${runRowId} --project ${project} "<что случилось; для вопроса — явно ЧТО РЕШИТЬ>"`,
    `   Блокер/вопрос — тем же ходом поставь рану Needs attention: uwuwu-cli task edit ${runRowId} --project ${project} --status "Needs attention".`,
    "   Пиши от себя (коммент идёт с бот-аккаунта, автор и так виден).",
    "4. Коммент не ушёл (ран/тело сняты в полёте) — повторов и запасных мест нет: заверши ход молча.",
  ].join("\n")
}