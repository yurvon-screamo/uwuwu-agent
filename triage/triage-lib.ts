// triage-lib.ts — чистые функции uwuwu-triage: разбор ТГ-апдейтов, состояние,
// извлечение финала прогона, сборка промпта. Без сети и токенов (тестируемо).

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

/** Заявка владельца, прошедшая фильтр. */
export interface Request {
  text: string
  chatID: number
  msgID: number
  receivedAt: number // ms
}

/** Активный прогон триажа (персистится — рестарт демона не теряет прогон). */
export interface RunState {
  sessionID: string
  startedAt: number // ms, момент отправки prompt
  req: Request
}

export interface TriageState {
  offset: number
  chatID?: number
  queue: Request[]
  run?: RunState
}

// ——— состояние ———

const isReq = (v: unknown): v is Request => {
  if (v === null || typeof v !== "object") return false
  const r = v as Record<string, unknown>
  return typeof r.text === "string" && r.text.trim() !== ""
    && Number.isFinite(r.chatID) && Number.isFinite(r.msgID)
}

/** Санитайз state.json: битые поля отбрасываем, always-on демон не роняем. */
export function validState(raw: unknown): TriageState {
  const out: TriageState = { offset: 0, queue: [] }
  if (raw === null || typeof raw !== "object") return out
  const r = raw as Record<string, unknown>
  if (Number.isFinite(r.offset) && (r.offset as number) >= 0) out.offset = r.offset as number
  if (Number.isFinite(r.chatID)) out.chatID = r.chatID as number
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

// ——— финал прогона ———

export interface SessionMsg {
  type?: string
  time?: { created?: number }
  content?: { type?: string; text?: string }[]
}

/** Финал прогона: сообщения API отдаёт DESC — берём первое assistant-сообщение
 *  новее старта прогона, текст = склейка его text-частей. "" = финала нет.
 *  (Ранняя assistant-реплика между тулами — «думает вслух», не финал: их
 *  пропускаем только если они старше startedAt; первый же новый assistant —
 *  ответ хода, у триаж-агента ход один.) */
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

// ——— промпт ———

/** Промпт прогона: методичка + заявка в одноразовых маркерах. Текст заявки —
 *  недоверенный вход: маркеры с nonce нельзя подделать изнутри заявки. */
export function buildPrompt(instructions: string, text: string, nonce: string): string {
  return `${instructions.trim()}\n\n[заявка ·${nonce}]\n${text}\n[конец ·${nonce}]\n\nРазбери заявку по методике и ответь сводкой.`
}

/** Заголовок сессии: первая содержательная строка заявки. */
export function runTitle(text: string): string {
  const first = text.split(/\r?\n/).map((l) => l.trim()).find((l) => l !== "") ?? "заявка"
  return `📥 ${first.slice(0, 60)}`
}

// ——— доставка в ТГ ———

const cutSafe = (s: string, end: number): number => {
  if (end >= s.length) return end
  const prev = s.charCodeAt(end - 1)
  return prev >= 0xd800 && prev <= 0xdbff ? end + 1 : end // не рвать суррогатную пару
}

/** Нарезка под лимит ТГ (одиночный суррогат = reject sendMessage). */
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
