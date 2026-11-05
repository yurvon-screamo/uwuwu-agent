// cron-lib.ts — чистые функции uwuwu-cron: расписания, промпты, состояние.
// Schedule: cron-выражение (5 полей), интервал (5m/2h/1d) или разовое at HH:MM.

export interface CronRow {
  rowId: string
  bodyDoc: string
  title: string
  schedule: string
  status: string
  lastRun: string
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
    return {
      kind: "at", hour, minute,
      ...(m[3] !== undefined ? { day: Number(m[3]), month: Number(m[4]) } : {}),
    }
  }
  const fields = t.split(/\s+/)
  if (fields.length !== 5) return null
  // каждое поле: запятая-список из (*|N[-M])[/step] — мусор вида "abc"
  // иначе парсится «легально» и молча не срабатывает никогда
  const partRe = /^(\*|\d+(?:-\d+)?)(?:\/\d+)?$/
  const ranges: Array<[number, number]> = [[0, 59], [0, 23], [1, 31], [1, 12], [0, 6]]
  const valid = fields.every((f, i) => {
    const [min, max] = ranges[i]!
    return f.split(",").every(p => {
      if (!partRe.test(p) || p.endsWith("/")) return false
      const m = /^(\*|\d+)(?:-(\d+))?/.exec(p)!
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

/** Разовое расписание (at): после срабатывания демон ставит Status → fired. */
export function isOneShot(sched: Sched): boolean {
  return sched.kind === "at"
}

/** Решение по свежему срезу доски прямо перед спавном: строка могла быть снята
 *  (cron rm/clean) или паузнута, пока тик обрабатывал очередь. Гонка «rm ↔ файр»
 *  (живой кейс 05.11: файр по строке, снятой в ходе тика) сужается до окна
 *  render → POST. */
export function fireGate(rows: CronRow[], rowId: string): "fire" | "removed" | "paused" {
  const row = rows.find((r) => r.rowId === rowId)
  if (row === undefined) return "removed"
  return row.status === "active" ? "fire" : "paused"
}

export function minuteStart(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes()).getTime()
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

/** Снятие YAML-frontmatter, HTML-комментариев и zero-width символов: экспорт
 *  тела, состоящего только из них, рендерится агенту в пустоту — инцидентный
 *  класс «тело из unsupported-комментариев → сессия с пустым промптом». */
export function stripNonContent(md: string): string {
  let text = md.replace(/^---\n[\s\S]*?\n---\n/, "")
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

// ——— промпты ———

export function sanitizeBoundary(text: string): string {
  return text.replace(/^\s*===.*$/gm, "≡")
}

/** Промпт срабатывания: тело крона + тихий протокол тика
 *  (коммент — только исключение; факт тика демон пишет в Last Run сам).
 *  Тело непусто по инварианту тика: гейт резолвит и валидирует его до
 *  спавна — пустое/unsupported-тело не доходит до промпта (refuse). */
export function firePrompt(row: CronRow, body: string, project: string): string {
  return [
    `Ты — исполнитель крон-задачи «${row.title.replace(/[\r\n]+/g, " ")}» (расписание: ${row.schedule}).`,
    "",
    "=== ТЕЛО КРОНА ===",
    sanitizeBoundary(body),
    "=== КОНЕЦ ТЕЛА ===",
    "",
    "ПРАВИЛА:",
    "1. Выполни задачу из тела. Это периодическая проверка/работа — работай автономно.",
    "2. Здоровый тик — тихий: никаких отчётных комментов и доков-отчётов; факт тика демон сам пишет в Last Run доски. Правки и записи, которые предписывает само тело крона (обновить док, заполнить ячейку), — нормальны; запрещён только шум ради отчёта.",
    "3. Коммент — ТОЛЬКО исключение (падение проверки, блокер, вопрос человеку):",
    `   uwuwu-cli task comment ${row.rowId} --project ${project} "<что случилось; для вопроса — явно ЧТО РЕШИТЬ>"`,
    "   Пиши от себя (коммент идёт с бот-аккаунта, автор и так виден).",
    "4. Коммент не ушёл (строка/тело сняты в полёте) — повторов и запасных мест нет: заверши ход молча.",
  ].join("\n")
}

/** Заголовок док-тела нового крона (стаб при создании). */
export function cronBodyStub(title: string, schedule: string): string {
  return [
    `# ${title}`,
    "",
    `Расписание: \`${schedule}\` · Status: active/paused в колонке доски.`,
    "",
    "## Что делать на каждом срабатывании",
    "(опиши работу агента)",
    "",
    "## Комменты",
    "_Здоровый тик тихий (факт тика — в Last Run). Коммент — только исключение:_",
    "_падение/блокер/вопрос владельцу (uwuwu-cli task comment) — в эту секцию._",
  ].join("\n")
}
