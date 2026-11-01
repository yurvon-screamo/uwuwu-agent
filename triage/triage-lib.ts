// triage-lib.ts — чистые функции uwuwu-triage: разбор ТГ-апдейтов, состояние,
// извлечение финала хода. Без сети и токенов (тестируемо).

export interface TgMessage {
  message_id: number
  date: number
  chat: { id: number; type: string }
  from?: { username?: string; is_bot?: boolean }
  text?: string
  caption?: string
}

export interface TgUpdate {
  update_id: number
  message?: TgMessage
}

/** Сообщение владельца, прошедшее фильтр. */
export interface Request {
  text: string
  chatID: number
  msgID: number
  receivedAt: number // ms
  attempts?: number
}

/** Активный ход в вечной сессии (персистится — рестарт демона не теряет ход).
 *  startedAt = 0 → промпт ещё не подтверждён (окно отправки). */
export interface RunState {
  sessionID: string
  startedAt: number // ms, момент отправки prompt
  req: Request
}

export interface TriageState {
  offset: number
  chatID?: number
  /** вечная сессия диалога — единый контекст всей переписки */
  session?: string
  /** sha256 методички, которую сессия уже получила (AGENTS.md при создании
   *  или последняя инъекция) — рассинхрон с prompt.md → инъекция обновления */
  instructionsHash?: string
  /** подряд оборванных финалов (outcome ≠ succeeded): 3 → пересоздание сессии */
  broken?: number
  queue: Request[]
  run?: RunState
}

// ——— состояние ———

const isReq = (v: unknown): v is Request => {
  if (v === null || typeof v !== "object") return false
  const r = v as Record<string, unknown>
  const attemptsOk = r.attempts === undefined
    || (Number.isInteger(r.attempts) && (r.attempts as number) >= 0)
  return typeof r.text === "string" && r.text.trim() !== ""
    && Number.isFinite(r.chatID) && Number.isFinite(r.msgID) && attemptsOk
}

/** Санитайз state.json: битые поля отбрасываем, always-on демон не роняем. */
export function validState(raw: unknown): TriageState {
  const out: TriageState = { offset: 0, queue: [] }
  if (raw === null || typeof raw !== "object") return out
  const r = raw as Record<string, unknown>
  if (Number.isInteger(r.offset) && (r.offset as number) >= 0) out.offset = r.offset as number
  if (Number.isFinite(r.chatID)) out.chatID = r.chatID as number
  if (typeof r.session === "string" && r.session.startsWith("ses_")) out.session = r.session
  if (typeof r.instructionsHash === "string" && r.instructionsHash.length <= 64) {
    out.instructionsHash = r.instructionsHash
  }
  if (Number.isInteger(r.broken) && (r.broken as number) >= 0 && (r.broken as number) <= 99) {
    out.broken = r.broken as number
  }
  if (Array.isArray(r.queue)) out.queue = r.queue.filter(isReq)
  const run = r.run
  if (run !== null && typeof run === "object") {
    const rn = run as Record<string, unknown>
    if (typeof rn.sessionID === "string" && rn.sessionID.startsWith("ses_")
        && Number.isFinite(rn.startedAt) && isReq(rn.req)) {
      out.run = {
        sessionID: rn.sessionID,
        startedAt: rn.startedAt as number,
        req: rn.req as Request,
      }
    }
  }
  return out
}

// ——— апдейты ТГ ———

export type UpdateOutcome =
  | { kind: "skip" } // не владелец / бот / не сообщение — тихо
  | { kind: "help"; msg: TgMessage } // /start | /help
  | { kind: "media"; msg: TgMessage } // медиа без текста
  | { kind: "req"; msg: TgMessage; text: string }

/** Разбор апдейта. Владелец один (owner по username) — остальные входы тихо
 *  скипаются: бот публичный, чужим не отвечаем и не раскрываем формат. */
export function classifyUpdate(u: TgUpdate, owner: string): UpdateOutcome {
  const m = u.message
  if (m === undefined) return { kind: "skip" }
  if (m.from?.is_bot === true) return { kind: "skip" }
  if (m.from?.username !== owner) return { kind: "skip" }
  if (m.chat.type !== "private") return { kind: "skip" }
  const t = (m.text ?? m.caption ?? "").trim()
  if (t === "") return { kind: "media", msg: m }
  if (t === "/start" || t === "/help") return { kind: "help", msg: m }
  return { kind: "req", msg: m, text: t }
}

// ——— финал хода ———

export interface SessionMsg {
  type?: string
  time?: { created?: number }
  content?: { type?: string; text?: string }[]
}

/** Финал хода: сообщения API отдаёт DESC — берём первое assistant-сообщение
 *  новее старта хода, текст = склейка его text-частей. "" = финала нет. */
export function extractFinal(msgs: SessionMsg[], startedAt: number): string {
  for (const m of msgs) {
    if (m.type !== "assistant") continue
    if ((m.time?.created ?? 0) < startedAt) break // дальше только старьё
    const text = (m.content ?? [])
      .filter((p) => p?.type === "text" && typeof p.text === "string")
      .map((p) => p.text as string)
      .join("\n")
      .trim()
    if (text !== "") return text
  }
  return ""
}

const textOf = (m: SessionMsg): string =>
  (m.content ?? [])
    .filter((p) => p?.type === "text" && typeof p.text === "string")
    .map((p) => p.text as string)
    .join("\n")
    .trim()

/** Дошёл ли наш промпт до сессии: user-сообщение новее границы с ЭТИМ
 *  текстом (точное или префикс-80 — длинные промпты могли пережать на
 *  несколько сообщений) → его время создания (точный startedAt хода:
 *  ответы строго после него), иначе null. Текст — дискриминатор от промпта
 *  предыдущего хода: быстрый double-text ≤2с ловился бы на голой границе. */
export function promptLandedAt(msgs: SessionMsg[], sinceMs: number, text: string): number | null {
  const needle = text.trim()
  const prefix = needle.slice(0, 80)
  let found: number | null = null
  for (const m of msgs) {
    if (m.type !== "user") continue
    const t = m.time?.created ?? 0
    if (t < sinceMs) continue
    const body = textOf(m)
    if (body !== needle && !body.startsWith(prefix)) continue
    if (found === null || t < found) found = t
  }
  return found
}

// ——— заголовок ———

/** Заголовок вечной сессии диалога. */
export const SESSION_TITLE = "📥 Триаж uwuwu"

// ——— доставка в ТГ ———

/** Граница среза, не рвущая суррогатную пару (4-байтные эмодзи): одиночный
 *  суррогат в sendMessage = reject TG-API. */
const cutSafe = (s: string, end: number): number => {
  if (end >= s.length) return end
  const prev = s.charCodeAt(end - 1)
  return prev >= 0xd800 && prev <= 0xdbff ? end + 1 : end
}

/** Нарезка под лимит ТГ. */
export function plainChunks(s: string, limit = 3800): string[] {
  const out: string[] = []
  let i = 0
  while (i < s.length) {
    const end = cutSafe(s, Math.min(i + limit, s.length))
    out.push(s.slice(i, end))
    i = end
  }
  return out
}

/** Выравнивание html-частей под куски исходного текста (plain-фолбэк
 *  доставки): при провале HTML-части досылаем ровно соответствующий кусок
 *  исходника — уже отправленное не дублируется, теги в чат не утекают. */
export function alignChunks(htmlParts: string[], source: string): string[] {
  const total = htmlParts.reduce((s, p) => s + p.length, 0) || 1
  let pos = 0
  return htmlParts.map((p) => {
    let end = pos + Math.ceil((p.length / total) * source.length)
    if (end > source.length) end = source.length
    end = cutSafe(source, end)
    const slice = source.slice(pos, end)
    pos = end
    return slice
  })
}
