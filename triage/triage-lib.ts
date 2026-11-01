// triage-lib.ts — чистые функции uwuwu-triage: разбор ТГ-апдейтов, состояние,
// извлечение финала хода. Без сети и токенов (тестируемо).

export interface TgMessage {
  message_id: number
  date: number
  chat: { id: number; type: string }
  from?: { username?: string; is_bot?: boolean }
  text?: string
  caption?: string
  photo?: { file_id?: string }[]
  document?: { file_id?: string; file_name?: string }
}

export interface TgUpdate {
  update_id: number
  message?: TgMessage
  edited_message?: TgMessage
}

/** Вложение заявки: файл уже выгружен в ФС (демон), путь — для агента
 *  (прикладывает к задаче через uwuwu-cli --attach). */
export interface Attachment {
  path: string
  name: string
}

/** Сообщение владельца, прошедшее фильтр. attempts — число неудачных попыток
 *  поднять прогон (poison-защита очереди); msgIDs — состав батча (склейка
 *  потока сообщений) для удаления из очереди одним юнитом. */
export interface Request {
  text: string
  chatID: number
  msgID: number
  receivedAt: number // ms
  attempts?: number
  msgIDs?: number[]
  attachments?: Attachment[]
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
  /** подряд оборванных финалов (outcome ≠ succeeded): 3 → пересоздание сессии */
  broken?: number
  /** ms последнего пересоздания по деградации + пауза обработки до:
   *  circuit breaker от спам-цикла пересозданий */
  lastResetAt?: number
  pausedUntil?: number
  queue: Request[]
  run?: RunState
  /** ms последнего push/редакта в очередь — опора дебаунса батча */
  lastPushAt?: number
  /** батч в полёте (взят из очереди, промпт ещё не подтверждён): edit-ветка
   *  отличает «ещё в очереди — можно править» от «уже пошло агенту» */
  firing?: { at: number; msgIDs: number[] }
}

// ——— состояние ———

const isReq = (v: unknown): v is Request => {
  if (v === null || typeof v !== "object") return false
  const r = v as Record<string, unknown>
  const attemptsOk = r.attempts === undefined
    || (Number.isInteger(r.attempts) && (r.attempts as number) >= 0)
  const msgIDsOk = r.msgIDs === undefined
    || (Array.isArray(r.msgIDs) && (r.msgIDs as unknown[]).length <= 100
      && (r.msgIDs as unknown[]).every((m) => Number.isInteger(m)))
  const attOk = r.attachments === undefined
    || (Array.isArray(r.attachments) && (r.attachments as unknown[]).length <= 20
      && (r.attachments as unknown[]).every((a) => {
        if (a === null || typeof a !== "object") return false
        const x = a as Record<string, unknown>
        return typeof x.path === "string" && x.path.length <= 512
          && typeof x.name === "string" && x.name.length <= 200
      }))
  return typeof r.text === "string" && r.text.trim() !== ""
    && Number.isFinite(r.chatID) && Number.isFinite(r.msgID) && attemptsOk && msgIDsOk && attOk
}

/** Санитайз state.json: битые поля отбрасываем, always-on демон не роняем. */
export function validState(raw: unknown): TriageState {
  const out: TriageState = { offset: 0, queue: [] }
  if (raw === null || typeof raw !== "object") return out
  const r = raw as Record<string, unknown>
  if (Number.isInteger(r.offset) && (r.offset as number) >= 0) out.offset = r.offset as number
  if (Number.isFinite(r.chatID)) out.chatID = r.chatID as number
  if (typeof r.session === "string" && r.session.startsWith("ses_")) out.session = r.session
  if (Number.isInteger(r.broken) && (r.broken as number) >= 0 && (r.broken as number) <= 99) {
    out.broken = r.broken as number
  }
  for (const k of ["lastResetAt", "pausedUntil", "lastPushAt"] as const) {
    const v = r[k]
    if (Number.isFinite(v) && (v as number) >= 0) out[k] = v as number
  }
  const firing = r.firing
  if (firing !== null && typeof firing === "object") {
    const f = firing as Record<string, unknown>
    if (Number.isFinite(f.at) && (f.at as number) >= 0 && Array.isArray(f.msgIDs)
      && (f.msgIDs as unknown[]).length <= 100 && (f.msgIDs as unknown[]).every((m) => Number.isInteger(m))) {
      out.firing = { at: f.at as number, msgIDs: f.msgIDs as number[] }
    }
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
  | { kind: "reset"; msg: TgMessage } // /reset — пересоздать сессию диалога
  | { kind: "media"; msg: TgMessage } // неподдерживаемое медиа (голос/видео/стикер)
  | { kind: "req"; msg: TgMessage; text: string } // photo/document идут заявкой (text может быть пустым)
  | { kind: "edit"; msg: TgMessage; text: string } // edited_message от владельца

/** Разбор апдейта. Владелец один (owner по username) — остальные входы тихо
 *  скипаются: бот публичный, чужим не отвечаем и не раскрываем формат. */
export function classifyUpdate(u: TgUpdate, owner: string): UpdateOutcome {
  const isEdit = u.message === undefined && u.edited_message !== undefined
  const m = u.message ?? u.edited_message
  if (m === undefined) return { kind: "skip" }
  if (m.from?.is_bot === true) return { kind: "skip" }
  if (m.from?.username !== owner) return { kind: "skip" }
  if (m.chat.type !== "private") return { kind: "skip" }
  const t = (m.text ?? m.caption ?? "").trim()
  const hasAttachment = (Array.isArray(m.photo) && m.photo.length > 0) || m.document?.file_id !== undefined
  if (isEdit) return t === "" && !hasAttachment ? { kind: "skip" } : { kind: "edit", msg: m, text: t }
  if (t === "" && !hasAttachment) return { kind: "media", msg: m }
  if (t === "/start" || t === "/help") return { kind: "help", msg: m }
  if (t === "/reset") return { kind: "reset", msg: m }
  return { kind: "req", msg: m, text: t }
}

// ——— батчинг потока сообщений ———

/** Когда головной батч готов: тишина DELAY после последнего push/редакта
 *  (поток писел склеивается целиком), но самое старое сообщение не ждёт
 *  дольше MAX (бесконечный поток не откладывает обработку навсегда).
 *  null — очередь пуста. */
export function batchReadyAt(queue: Request[], lastPushAt: number, delayMs: number, maxMs: number): number | null {
  const head = queue[0]
  if (head === undefined) return null
  const last = Math.max(lastPushAt, head.receivedAt)
  return Math.min(last + delayMs, head.receivedAt + maxMs)
}

/** Головной батч: всё, что пришло ДО момента готовности (сообщения,
 *  прилетевшие позже, — следующий батч, батчи не смешиваются). */
export function takeBatch(queue: Request[], readyAt: number): Request[] {
  const out: Request[] = []
  for (const r of queue) {
    if (r.receivedAt > readyAt) break
    out.push(r)
  }
  return out
}

/** Текст заявки: слова юзера + маркеры вложений (путь в ФС — источник для
 *  --attach агента) + маркеры неудач выгрузки. */
export function composeText(text: string, attachments: Attachment[], failed: string[]): string {
  return [
    text,
    ...attachments.map((a) => `[вложение: ${a.name} — файл в ФС: ${a.path}]`),
    ...failed.map((n) => `[вложение ${n} не выгрузилось — попроси юзера продублировать]`),
  ].filter((s) => s !== "").join("\n")
}

/** Батч как один ход диалога: тексты склеены, вложения объединены, msgIDs —
 *  состав для удаления из очереди одним юнитом (dead-letter/recovery). */
export function joinBatch(items: Request[]): Request {
  const head = items[0]!
  const attachments = items.flatMap((i) => i.attachments ?? [])
  return {
    text: items.map((i) => i.text).join("\n\n"),
    chatID: head.chatID,
    msgID: head.msgID,
    receivedAt: head.receivedAt,
    ...(items.length > 1 ? { msgIDs: items.map((i) => i.msgID) } : {}),
    ...(attachments.length > 0 ? { attachments: attachments.slice(0, ATT_BATCH_MAX) } : {}),
  }
}

/** Кап вложений батча: маркеры в тексте и files-состав не расходятся. */
export const ATT_BATCH_MAX = 20

/** Удалить из очереди все сообщения батча (по msgID). */
export function dropBatch(queue: Request[], req: Request): Request[] {
  const ids = new Set(req.msgIDs ?? [req.msgID])
  return queue.filter((r) => !ids.has(r.msgID))
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
