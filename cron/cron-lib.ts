// cron-lib.ts — чистые функции uwuwu-cron: расписания, промпты, состояние.
// Schedule: cron-выражение (5 полей), интервал (5m/2h/1d) или разовое at HH:MM.

export interface CronRow {
  rowId: string
  bodyDoc: string
  title: string
  /** Расписание из custom-schedule дока-тела; пусто для обычных задач. */
  schedule: string
  /** Имя базового агента вечной сессии (custom-agent дока-тела);
   *  пусто — изолированные спавны (исторический контур). */
  agent: string
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
  /** Файр вечной сессии с неподтверждённым POST: {rowId, prompt, attemptAt}.
   *  Ретрай обязан дедупить приземление по тексту промпта (паттерн hub
   *  steerLandedAt) — дубль-инструкция в вечный контекст не доезжает.
   *  Дропится при пересоздании сессии (файр начнётся заново). */
  pendingRun?: { rowId: string; prompt: string; attemptAt: number }
  /** ms последней admission-попытки автокомпакта вечной сессии (пишется ДО
   *  POST — коулдаун против повторного admission переживает рестарт). */
  lastCompactAt?: number
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

/** Гард-лимит тайтла сессии opencode: строка целиком (паритет factory-lib). */
export const SESSION_TITLE_MAX = 80

/** Тайтл ран-карточки и файр-сессии: «⏰ <номер> — <тайтл мастера> · <дата>»
 *  (номер = номер issue). Пустой тайтл мастера — номер без «—» (легаси-вид).
 *  Секунды в дате делают тайтл уникальным на доске (дубли строк CLI ловит
 *  как ошибку). CRLF → пробел, pathSafe. */
export function runCardTitle(master: { rowId: string; title: string }, now: Date): string {
  const sec = String(now.getSeconds()).padStart(2, "0")
  const safe = pathSafe(master.title.replace(/[\r\n]+/g, " ").trim())
  const head = safe === "" ? master.rowId : `${master.rowId} — ${safe}`
  return `⏰ ${head} · ${formatLocal(now)}:${sec}`
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
  // never-fired интервал: due файрит следующим тиком — плановое время now
  // (now+interval появился бы только после файра)
  if (prevFire === 0 && sched.kind === "interval") {
    const plannedMs = minuteStart(now)
    if (row.plannedMs !== plannedMs) patch.planned = formatLocal(now)
    return patch
  }
  // гард вечного молчуна: полный годовой скан дорог только в cron-ветке
  // nextFire (интервал/at — O(1), считаются всегда). never-fired cron-мастер
  // с пустой клеткой не сканируем: due файрит на первом совпадении, после
  // файра reconcile посчитает next от prevFire. Без гарда «0 0 30 2 *» гонял
  // бы скан каждым тиком вечно
  if (prevFire === 0 && row.plannedMs === null && sched.kind === "cron") return patch
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
  const pending = t.pendingRun
  const pendingOk = pending === undefined || (
    pending !== null && typeof pending === "object" &&
    typeof (pending as Record<string, unknown>).rowId === "string" &&
    typeof (pending as Record<string, unknown>).prompt === "string" &&
    typeof (pending as Record<string, unknown>).attemptAt === "number"
  )
  return (
    (t.sessionID === "" || (typeof t.sessionID === "string" && t.sessionID.startsWith("ses_"))) &&
    typeof t.lastFiredAt === "number" &&
    (t.firedAtIso === undefined || typeof t.firedAtIso === "string") &&
    (t.lastRefusal === undefined || typeof t.lastRefusal === "string") &&
    (t.lastCompactAt === undefined || (typeof t.lastCompactAt === "number" && t.lastCompactAt >= 0)) &&
    pendingOk
  )
}

// ——— резолв и валидация тела ———

/** Тайтл → безопасный сегмент hpath (конвенция CLI path_safe: '/' → U+2215). */
export function pathSafe(title: string): string {
  return title.replace(/\//g, "∕")
}

/** Снятие экспорт-обвязки и невидимых символов: YAML-frontmatter, заголовок-эхо
 *  дока (`# <title>` первой строкой — метаданные носителя, не
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
    "2. Здоровый ран — тихий: никаких отчётных комментов и доков-отчётов; ран-карточка — запись запуска. Правки и записи, которые предписывает само тело задачи, — нормальны; запрещён только шум ради отчёта.",
    "3. Финал: успешный прогон — закрой ран сам:",
    `   uwuwu-cli task edit ${runRowId} --project ${project} --status Done`,
    "   (ран — запись запуска, не задача: приёмки успешного тика не требуется). Не уверен в исходе или ход оборвался — не закрывай: ран, оставшийся в Todo после тика, — сигнал сбоя.",
    "4. Коммент — ТОЛЬКО исключение (падение проверки, блокер, вопрос человеку):",
    `   uwuwu-cli task comment ${runRowId} --project ${project} "<что случилось; для вопроса — явно ЧТО РЕШИТЬ>"`,
    `   Блокер/вопрос — тем же ходом поставь рану Needs attention: uwuwu-cli task edit ${runRowId} --project ${project} --status "Needs attention".`,
    "   Пиши от себя (коммент идёт с бот-аккаунта, автор и так виден).",
    "5. Коммент не ушёл (ран/тело сняты в полёте) — повторов и запасных мест нет: заверши ход молча.",
  ].join("\n")
}

// ——— вечная сессия (custom-agent) ———

/** Решение по статусу сессии мастера: idle → новый ход, running → стирт
 *  (нативный delivery:"steer", паттерн hub), gone → пересоздание сессии
 *  (контекст вечного мастера потерян — честно в логе вызывающего). */
export function decideFire(status: "idle" | "running" | "gone"): "prompt" | "steer" | "recreate" {
  switch (status) {
    case "idle": return "prompt"
    case "running": return "steer"
    case "gone": return "recreate"
  }
}

/** Дрейф агента живой вечной сессии против custom-agent мастера (эпизод
 *  09.11: агент вшит при createAgentSession, смена карточки сессию не
 *  обновляет — первый файр POST-ится 200, ход умирает «Agent not found»,
 *  ретрая нет). idle → recreate (gone-путь с честным логом), running →
 *  defer (живой ход не рвём — пересоздание на следующем файре, умерший
 *  ход читается как idle и ловится там же). agent null (сбой чтения/не
 *  отдал сервер) или совпадение — null: ложных recreate нет. */
export function agentDriftAction(
  status: "idle" | "running",
  sessionAgent: string | null,
  masterAgent: string,
): "recreate" | "defer" | null {
  if (sessionAgent === null || sessionAgent === masterAgent) return null
  return status === "idle" ? "recreate" : "defer"
}

/** Причина отказа файра вечного мастера; null — деф на месте. */
export function resolveAgentIssue(agent: string, defExists: boolean): string | null {
  if (agent === "") return "custom-agent пуст"
  if (!defExists) {
    return `деф агента «${agent}» не найден в ~/.config/opencode/agents/<name>.md — заведи деф или сними свойство (task edit <id> --agent "")`
  }
  return null
}

export interface AgentModel {
  providerID: string
  id: string
  variant?: string
}

/** Модель из frontmatter дефа агента: opencode не применяет модель дефа
 *  к primary-сессиям — демон передаёт явно. Оба живых формата: инлайн
 *  `model: provider/id#variant` (tester) и раздельные ключи `model` +
 *  `variant` (dreamer, lead); без model — null (модель демона у вызывающего). */
export function parseAgentDefModel(defMd: string): AgentModel | null {
  const fm = /^---\n([\s\S]*?)\n---/.exec(defMd)?.[1]
  if (fm === undefined) return null
  const modelLine = /^model:\s*(\S+)\s*$/m.exec(fm)?.[1]
  if (modelLine === undefined) return null
  const [providerID, id] = modelLine.split("/")
  if (providerID === undefined || id === undefined) return null
  const [modelId, hashVariant] = id.split("#")
  if (modelId === undefined || modelId === "") return null
  const variant = hashVariant ?? /^variant:\s*(\S+)\s*$/m.exec(fm)?.[1]
  return {
    providerID,
    id: modelId,
    ...(variant !== undefined && variant !== "" ? { variant } : {}),
  }
}

/** Срез сообщений сессии (форма REST opencode; user-сообщения несут текст
 *  верхним уровнем, assistant — content-частями). */
export interface SessionMsg {
  id?: string
  type?: string
  time?: { created?: number }
  text?: string
  content?: { type?: string; text?: string }[]
}

const msgText = (m: SessionMsg): string => {
  if (typeof m.text === "string" && m.text.trim() !== "") return m.text.trim()
  return (m.content ?? [])
    .filter((p) => p?.type === "text" && typeof p.text === "string")
    .map((p) => p.text as string)
    .join("\n")
    .trim()
}

/** Приземлился ли промпт: user-сообщение новее границы с этим текстом
 *  (точным или префиксом-80 — длинные промпты могли пережать на части) →
 *  его time.created; иначе null. Порт hub promptLandedAt — дедуп приземления
 *  при ретрае pendingRun («сервер приземлил, ответ потерян»). */
export function promptLandedAt(msgs: SessionMsg[], sinceMs: number, prompt: string): number | null {
  const needle = prompt.trim()
  const prefix = needle.slice(0, 80)
  let found: number | null = null
  for (const m of msgs) {
    if (m.type !== "user") continue
    const at = m.time?.created ?? 0
    if (at < sinceMs) continue
    const body = msgText(m)
    if (body !== needle && !body.startsWith(prefix)) continue
    if (found === null || at < found) found = at
  }
  return found
}

/** Приземлился ли конверт рана: user-сообщение новее границы с вхождением
 *  runRowId → его time.created; иначе null. Сигнатура вместо полного текста
 *  (promptLandedAt): rowId вшит в конверт шаблоном agentFirePrompt и стабилен
 *  между попытками доставки — ручная копия с разошедшимся текстом (другое
 *  расписание, frontmatter) тем же дедупом признаётся, дубль-инструкция в
 *  вечный контекст не доезжает (ADR-075). rowId уникален (timestamp+random),
 *  случайное вхождение в чужой текст нечужно. */
export function runLandedAt(msgs: SessionMsg[], sinceMs: number, runRowId: string): number | null {
  let found: number | null = null
  for (const m of msgs) {
    if (m.type !== "user") continue
    const at = m.time?.created ?? 0
    if (at < sinceMs) continue
    if (!msgText(m).includes(runRowId)) continue
    if (found === null || at < found) found = at
  }
  return found
}

/** Промпт файра вечной сессии: конверт самодостаточен — вечная сессия могла
 *  переехать после реконструкции (контекст пуст). Тело непусто по инварианту
 *  тика: гейт резолвит и валидирует его до файра (refuse). */
export function agentFirePrompt(
  master: { title: string; schedule: string },
  body: string,
  runRowId: string,
  project: string,
): string {
  return [
    `⏰ Файр крона «${master.title.replace(/[\r\n]+/g, " ")}» (расписание: ${master.schedule}).`,
    `Ран-карточка запуска — задача ${runRowId} на доске «Задачи ${project}»: она уже создана, работать по ней не нужно.`,
    "",
    "=== ИНСТРУКЦИЯ (тело мастера) ===",
    sanitizeBoundary(body),
    "=== КОНЕЦ ИНСТРУКЦИИ ===",
    "",
    "ПРАВИЛА РАНА:",
    "1. Выполни работу из инструкции автономно; работай по своему дефу.",
    "2. Отчёт прогона — комментом в ран-карточку (тело рана — снапшот инструкции на момент файра, его не редактируй).",
    "3. Успешный прогон — закрой ран сам:",
    `   uwuwu-cli task edit ${runRowId} --project ${project} --status Done`,
    "   Не уверен в исходе или ход оборвался — не закрывай: ран, оставшийся в Todo, — сигнал сбоя.",
    "4. Блокер/вопрос человеку — коммент в ран и Needs attention:",
    `   uwuwu-cli task comment ${runRowId} --project ${project} "<что случилось; для вопроса — явно ЧТО РЕШИТЬ>"`,
    `   uwuwu-cli task edit ${runRowId} --project ${project} --status "Needs attention"`,
  ].join("\n")
}