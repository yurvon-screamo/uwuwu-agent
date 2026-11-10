// lib/hub.ts — универсальное ядро ТГ-демона на вечной opencode-сессии
// (behavior-preserving перенос из triage/triage.ts + общие чистые функции из
// triage/triage-lib.ts; один процесс = один хаб).
//
// Пайп: ТГ long-poll + источники промптов (профиль: вотчер черновиков у
// триажа) → ОДНА вечная сессия диалога → инкрементальная
// доставка ответов в ТГ. Свободен → ход; занят → стирт (нативный
// delivery:"steer"). Очередь/батчи/стирт/доставка/рестарт-гарантии — как у
// триажа (см. triage/README: сообщение не теряется при рестарте, ответы
// at-least-once, poison → dead-letter).
//
// Внешних зависимостей нет: markdown → TG HTML инжектится входом
// (HubProfile.tgFormatter — md-to-telegram живёт в node_modules пакета-входа).
// Профиль несёт идентичность демона: пути/state/лог, деф-агент, тайтл сессии,
// HELP, env-крюки, дефолт модели (формат с variant — паритет dreamer: model
// из дефа к primary-сессиям не применяется, демон передаёт явно).

import {
  appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync,
} from "node:fs"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { acquireLock, branchLabel, detectBranch } from "./single-instance"
import { withE2ETag } from "./e2e-tag"
import { compactProbe, compactTick, sessionCompact, type CompactPolicy } from "./compact"

// ——— типы протокола ТГ ———

export interface TgMessage {
  message_id: number
  text?: string
  caption?: string
  date: number
  chat: { id: number; type: string }
  from?: { username?: string; is_bot?: boolean }
  photo?: { file_id: string }[]
  document?: { file_id: string; file_name?: string }
  video?: { file_id: string; file_name?: string }
  animation?: { file_id: string; file_name?: string }
  video_note?: { file_id: string }
  audio?: { file_id: string; file_name?: string }
  voice?: { file_id: string }
  sticker?: { file_id: string }
}

export interface TgUpdate {
  update_id: number
  message?: TgMessage
  edited_message?: TgMessage
}

export interface Attachment {
  path: string
  name: string
}

/** Медиа-вложение: файл выгружается демоном в MEDIA_DIR под именем `name`. */
export interface MediaFile {
  fileId: string
  name: string
}

// ——— заявка и состояние ———

export interface Request {
  text: string
  /** ТГ-чат доставки финала. 0 = доставки нет (вход без чата: черновик-вход
   *  с досок, крон-промпт до первичного /start владельца). */
  chatID: number
  /** ТГ message_id (>0); не-ТГ источники — синтетические отрицательные id
   *  демона (edit-ветка и dead-letter матчатся по id безусловно). */
  msgID: number
  receivedAt: number // ms
  attempts?: number
  msgIDs?: number[]
  attachments?: Attachment[]
  /** имена вложений, НЕ выгрузившихся при приёме (маркеры в тексте
   *  пересобираются при правке — знание о неудаче переживает edit) */
  failed?: string[]
  /** черновики задач в заявке (вотчер досок триажа); у ТГ-сообщений отсутствует */
  drafts?: DraftRef[]
  /** тихий финал допустим (крон-промпты): ход без текстовых ответов —
   *  не доставка «(пустой ответ)», а молчание (оборванный прогон — пометка). */
  allowSilent?: boolean
}

/** Черновик задачи со строки доски «Задачи <proj>» (вотчер демона). */
export interface DraftRef {
  project: string
  rowId: string
  /** док-тело черновика — источник updatedAt для дебаунса тишины */
  docId: string
  title: string
}

/** Вердикт агента по черновику (seen-ack). Канон — ровно эти токены: парсер
 *  и методичка agent.md согласованы, псевдонимов нет. */
export type DraftVerdict = "todo" | "merge" | "rm" | "пишется" | "обсуждение"

/** Полный канонический набор — валидация парсера и state.json. */
export const DRAFT_VERDICTS: readonly DraftVerdict[] = ["todo", "merge", "rm", "пишется", "обсуждение"]

/** Активный ход в вечной сессии (персистится — рестарт демона не теряет ход).
 *  startedAt = 0 → промпт ещё не подтверждён (окно отправки). */
export interface RunState {
  sessionID: string
  startedAt: number // ms, момент отправки prompt
  req: Request
  /** msg-id assistant-сообщений, уже доставленных в ТГ в этом ходе:
   *  дедуп инкрементальной доставки при повторных тиках и рестартах */
  delivered?: string[]
  /** time.created последнего приземлившегося стирта (из ответа POST /prompt).
   *  Граница «done» в sessionStatus: сервер всю фазу исполнения, начатого
   *  стиртом по idle-сессии, отдаёт стухший outcome и старый time.idle —
   *  без этой границы ход был бы объявлен готовым в первый тик. */
  steerAt?: number
}

/** Состояние демона (state.json). Поля черновик-трекера и крон-шедулера
 *  опциональны: каждый вход валидирует/потребляет свои (extra-хук validState). */
export interface HubState {
  offset: number
  chatID?: number
  /** вечная сессия диалога — единый контекст всей переписки */
  session?: string
  queue: Request[]
  run?: RunState
  /** ms последнего push/редакта в очередь — опора дебаунса батча */
  lastPushAt?: number
  /** ms последнего выхода демона (успешная доставка в ТГ / завершённый
   *  финал хода) — опора тайм-аута тишины выходного потока (silenceCheck);
   *  материализует createHub при загрузке */
  lastDeliveryAt?: number
  /** батч в полёте (взят из очереди, промпт ещё не подтверждён): edit-ветка
   *  отличает «ещё в очереди — можно править» от «уже пошло агенту» */
  firing?: { at: number; msgIDs: number[] }
  /** трекер черновиков досок (триаж): rowId → наблюдение вотчера */
  drafts?: Record<string, object>
  /** черновики, застрявшие в Draft после лимита ходов: не слать (триаж) */
  draftSkip?: Record<string, number>
  /** последний выданный синтетический msgID не-ТГ источников (отрицательные) */
  nextDraftMsgID?: number
  /** ms последней admission-попытки автокомпакта (пишется ДО POST —
   *  коулдаун против повторного admission переживает рестарт) */
  lastCompactAt?: number
}

const isDraftRef = (v: unknown): v is DraftRef => {
  if (v === null || typeof v !== "object") return false
  const x = v as Record<string, unknown>
  const str = (k: string, cap: number): boolean =>
    typeof x[k] === "string" && (x[k] as string).length <= cap
  return str("project", 64) && str("rowId", 128) && str("docId", 128) && str("title", 300)
}

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
  const silentOk = r.allowSilent === undefined || typeof r.allowSilent === "boolean"
  // drafts валидируется в validState (битое поле стрипится, заявка остаётся)
  return typeof r.text === "string" && r.text.trim() !== ""
    && Number.isFinite(r.chatID) && Number.isInteger(r.msgID)
    && attemptsOk && msgIDsOk && attOk && silentOk
}

/** Форма failed-поля (стрипится отдельно — битое поле не роняет заявку). */
const failedShapeOk = (v: unknown): boolean =>
  Array.isArray(v) && v.length <= 10 && v.every((n) => typeof n === "string" && n.length <= 200)

/** Санитайз state.json: битые поля отбрасываем, always-on демон не роняем.
 *  Специфичные поля входа (черновик-трекер триажа) дозаполняет
 *  extra-хук — битое поле хука стрипится, база не роняется. */
export function validState(
  raw: unknown,
  extra?: (raw: Record<string, unknown>, out: HubState) => void,
): HubState {
  const out: HubState = { offset: 0, queue: [] }
  if (raw === null || typeof raw !== "object") return out
  const r = raw as Record<string, unknown>
  if (Number.isInteger(r.offset) && (r.offset as number) >= 0) out.offset = r.offset as number
  if (Number.isFinite(r.chatID)) out.chatID = r.chatID as number
  if (typeof r.session === "string" && r.session.startsWith("ses_")) out.session = r.session
  if (Number.isFinite(r.lastPushAt) && (r.lastPushAt as number) >= 0) {
    out.lastPushAt = r.lastPushAt as number
  }
  if (Number.isFinite(r.lastDeliveryAt) && (r.lastDeliveryAt as number) >= 0) {
    out.lastDeliveryAt = r.lastDeliveryAt as number
  }
  const firing = r.firing
  if (firing !== null && typeof firing === "object") {
    const f = firing as Record<string, unknown>
    if (Number.isFinite(f.at) && (f.at as number) >= 0 && Array.isArray(f.msgIDs)
      && (f.msgIDs as unknown[]).length <= 100 && (f.msgIDs as unknown[]).every((m) => Number.isInteger(m))) {
      out.firing = { at: f.at as number, msgIDs: f.msgIDs as number[] }
    }
  }
  if (Array.isArray(r.queue)) {
    out.queue = r.queue.filter(isReq).map((q) => {
      // битный failed — стрип поля, не заявки (ручная правка state.json не
      // должна стоить юзеру сообщения)
      if (q.failed !== undefined && !failedShapeOk(q.failed)) {
        const { failed: _drop, ...rest } = q
        return rest
      }
      if (q.drafts !== undefined) {
        const ok = Array.isArray(q.drafts) && (q.drafts as unknown[]).length <= 20
          && (q.drafts as unknown[]).every(isDraftRef)
        if (!ok) {
          const { drafts: _drop, ...rest } = q
          return rest
        }
      }
      return q
    })
  }
  const run = r.run
  if (run !== null && typeof run === "object") {
    const rn = run as Record<string, unknown>
    if (typeof rn.sessionID === "string" && rn.sessionID.startsWith("ses_")
        && Number.isFinite(rn.startedAt) && isReq(rn.req)) {
      const outRun: RunState = {
        sessionID: rn.sessionID,
        startedAt: rn.startedAt as number,
        req: rn.req as Request,
      }
      // битый delivered — почастичный стрип: невалидные id выбрасываются,
      // валидные остаются (полная потеря переотправила бы ВСЕ ответы хода);
      // при переполнении остаются свежайшие (старьё доставлено давно)
      if (Array.isArray(rn.delivered)) {
        const ids = (rn.delivered as unknown[])
          .filter((x): x is string => typeof x === "string" && x.length > 0 && x.length <= 128)
          .slice(-100)
        if (ids.length > 0) outRun.delivered = ids
      }
      if (typeof rn.steerAt === "number" && Number.isFinite(rn.steerAt) && rn.steerAt >= 0) {
        outRun.steerAt = rn.steerAt
      }
      out.run = outRun
    }
  }
  if (Number.isFinite(r.lastCompactAt) && (r.lastCompactAt as number) >= 0) {
    out.lastCompactAt = r.lastCompactAt as number
  }
  extra?.(r, out)
  return out
}

// ——— liveness: тайм-аут тишины выходного потока ———
// Правило опыта «SiYuan-контур uwuwu» (Режим 5, дополнение): liveness-детектор
// демона-обработчика потока обязан смотреть на ВЫХОД (доставки/выходные
// записи), не только на процесс/journal-болтовню — «тишина исходящего потока»
// включает «живость процесса».

/** Минимальный интервал повторных ПОПЫТОК доставки алерта тишины, пока
 *  выходов нет (ТГ недоступен). Успешный алерт — это send-успех: он обновляет
 *  метку тишины сам, на success-пути окно не участвует. */
export const SILENCE_ALERT_RETRY_MS = 60 * 60_000

/** Окно ретрая недоставленного алерта — в памяти процесса (рестарт сбрасывает:
 *  метка тишины персистится, недоставленный алерт ретраится сразу). */
export interface SilenceGate {
  nextAlertAt: number
}

/** Проверка тишины: `alert = (age ≥ timeout) && (now ≥ gate.nextAlertAt)`.
 *  Свежая доставка (метка обновлена) гасит алерт первым условием — повторного
 *  алерта на живом потоке нет; недоставленный алерт метку не трогает, тишина
 *  продолжает расти, окно душит попытки до одной на retryMs (дефолт —
 *  SILENCE_ALERT_RETRY_MS; инъекция для e2e — прецедент ADR-061). */
export function silenceCheck(
  lastDeliveryAt: number,
  gate: SilenceGate,
  now: number,
  timeoutMs: number,
  retryMs: number = SILENCE_ALERT_RETRY_MS,
): { gate: SilenceGate; alert: boolean; ageMs: number } {
  const ageMs = now - lastDeliveryAt
  const alert = ageMs >= timeoutMs && now >= gate.nextAlertAt
  return {
    gate: alert ? { nextAlertAt: now + retryMs } : gate,
    alert,
    ageMs,
  }
}

// ——— апдейты ТГ ———

/** Имя файла вложения: message_id-префикс гарантирует уникальность внутри
 *  батча (два безымянных видео форварда-альбома не перезаписывают друг
 *  друга), слэши и управляющие символы имени — санитизируются. */
function attachmentName(messageId: number, fileName: string | undefined, fallback: string): string {
  const raw = fileName !== undefined && fileName.trim() !== "" ? fileName : fallback
  const safe = raw.replace(/[/\\]/g, "_").replace(/\p{Cc}/gu, "_").slice(0, 80)
  return `${messageId}_${safe}`
}

/** Медиа-вложения сообщения: что демон выгружает в ФС (photo/document/
 *  video/animation/video_note/audio). Единый маппинг для классификации и
 *  выгрузки — записи без file_id вложением не считаются (выгрузка
 *  невозможна по определению). Голосовые/стикеры не входят: отбиваются
 *  демоном явно (kind: "media"). */
export function mediaFiles(m: TgMessage): MediaFile[] {
  const out: MediaFile[] = []
  const add = (fileId: string | undefined, name: string): void => {
    if (fileId !== undefined) out.push({ fileId, name })
  }
  // фото: последний элемент массива = максимальное разрешение
  const photo = Array.isArray(m.photo) ? m.photo[m.photo.length - 1] : undefined
  add(photo?.file_id, `${m.message_id}.jpg`)
  add(m.document?.file_id, attachmentName(m.message_id, m.document?.file_name, "file"))
  add(m.video?.file_id, attachmentName(m.message_id, m.video?.file_name, "video.mp4"))
  add(m.animation?.file_id, attachmentName(m.message_id, m.animation?.file_name, "animation.mp4"))
  add(m.video_note?.file_id, `${m.message_id}.mp4`)
  add(m.audio?.file_id, attachmentName(m.message_id, m.audio?.file_name, "audio.mp3"))
  return out
}

export type UpdateOutcome =
  | { kind: "skip" } // не владелец / бот / не сообщение — тихо
  | { kind: "help"; msg: TgMessage } // /start | /help
  | { kind: "reset"; msg: TgMessage } // /reset — пересоздать сессию диалога
  | { kind: "media"; msg: TgMessage } // не-текстовый вход без выгружаемых вложений (голос/стикер и пр.) — bounce владельцу
  | { kind: "req"; msg: TgMessage; text: string } // текст и/или выгружаемые вложения идут заявкой (text может быть пустым)
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
  const hasAttachment = mediaFiles(m).length > 0
  if (isEdit) return t === "" && !hasAttachment ? { kind: "skip" } : { kind: "edit", msg: m, text: t }
  if (t === "" && !hasAttachment) return { kind: "media", msg: m }
  if (t === "/start" || t === "/help") return { kind: "help", msg: m }
  if (t === "/reset") return { kind: "reset", msg: m }
  return { kind: "req", msg: m, text: t }
}

// ——— батчинг потока сообщений ———

/** Когда головной батч готов: тишина DELAY после последнего push/редакта
 *  (поток писем склеивается целиком), но самое старое сообщение не ждёт
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

/** Текст заявки: слова юзера + маркеры вложений (путь в ФС — агент фиксирует
 *  его в теле задачи) + маркеры неудач выгрузки. */
export function composeText(text: string, attachments: Attachment[], failed: string[]): string {
  return [
    text,
    ...attachments.map((a) => `[вложение: ${a.name} — файл в ФС: ${a.path}]`),
    ...failed.map((n) => `[вложение ${n} не выгрузилось — попроси юзера продублировать]`),
  ].filter((s) => s !== "").join("\n")
}

/** Кап вложений батча: маркеры в тексте и files-состав не расходятся. */
export const ATT_BATCH_MAX = 20

/** Порция без-чатовых заявок в батче (черновики досок триажа): ТГ-заявки не
 *  режем (старое поведение), а без-чатовых берём максимум `max` —
 *  накопившийся бэклог одним ходом деградирует вечную сессию (контекст/
 *  компакция). Невошедшие остаются в очереди — следующий ход. */
export function capUnchatBatch(items: Request[], max: number): Request[] {
  return [
    ...items.filter((i) => i.chatID !== 0),
    ...items.filter((i) => i.chatID === 0).slice(0, Math.max(0, max)),
  ]
}

/** Батч как один ход диалога: тексты склеены, вложения объединены, msgIDs —
 *  состав для удаления из очереди одним юнитом (dead-letter/recovery).
 *  Доставка — по первому ненулевому chatID (чисто без-чатовый батч = 0,
 *  смешанный доставляется в ТГ как единый ход единого пайпа). */
export function joinBatch(items: Request[]): Request {
  const head = items[0]
  if (head === undefined) throw new Error("joinBatch: пустой батч")
  const withChat = items.find((i) => i.chatID !== 0)
  const attachments = items.flatMap((i) => i.attachments ?? [])
  const failed = [...new Set(items.flatMap((i) => i.failed ?? []))]
  const drafts = items.flatMap((i) => i.drafts ?? [])
  const silent = items.every((i) => i.allowSilent === true)
  return {
    text: items.map((i) => i.text).join("\n\n"),
    chatID: withChat !== undefined ? withChat.chatID : 0,
    msgID: head.msgID,
    receivedAt: head.receivedAt,
    ...(items.length > 1 ? { msgIDs: items.map((i) => i.msgID) } : {}),
    ...(attachments.length > 0 ? { attachments: attachments.slice(0, ATT_BATCH_MAX) } : {}),
    ...(failed.length > 0 ? { failed: failed.slice(0, 10) } : {}),
    ...(drafts.length > 0 ? { drafts: drafts.slice(0, 20) } : {}),
    ...(silent ? { allowSilent: true } : {}),
  }
}

/** Удалить из очереди все сообщения батча (по msgID). */
export function dropBatch(queue: Request[], req: Request): Request[] {
  const ids = new Set(req.msgIDs ?? [req.msgID])
  return queue.filter((r) => !ids.has(r.msgID))
}

// ——— стирт: вброс сообщения владельца в активный ход ———

/** Кандидат на стирт: ТГ-подмножество очереди (без-чатовые входы в стирт не
 *  входят — дождутся конца хода в очереди), тот же дебаунс, что у старта
 *  (тишина delayMs, кап maxMs от самого старого). null — ТГ-батча нет. */
export function steerCandidate(
  queue: Request[],
  lastPushAt: number,
  delayMs: number,
  maxMs: number,
): { batch: Request; readyAt: number } | null {
  const tg = queue.filter((r) => r.chatID !== 0)
  const readyAt = batchReadyAt(tg, lastPushAt, delayMs, maxMs)
  if (readyAt === null) return null
  const items = takeBatch(tg, readyAt)
  if (items.length === 0) return null
  return { batch: joinBatch(items), readyAt }
}

/** Батч краш-восстановления по персистентному firing-маркеру: точная персона
 *  стиртованного батча (msgIDs), НЕ пересборка по готовности — новое сообщение,
 *  пришедшее до рестарта, могло бы прицепиться к дедупу и быть потеряно или
 *  уйти агенту дважды вместе со стиртованным. null — персоны в очереди нет
 *  (маркер стух без батча). Порядок — как в очереди. */
export function steerRecoveryBatch(queue: Request[], msgIDs: number[]): Request | null {
  const ids = new Set(msgIDs)
  const items = queue.filter((r) => ids.has(r.msgID))
  return items.length > 0 ? joinBatch(items) : null
}

/** Признанные (admitted), но ещё не потреблённые стирты: эндпоинт /inbox
 *  показывает их раньше, чем они материализуются в /message (потребление —
 *  следующим шагом агент-лупа). Без этого дедуп рестарт-окна слеп в краше
 *  «POST приземлился — save не пережил» и пере-POST-ил бы уже приземлённый
 *  стирт (эмпирика 2.2.5: ответ POST = admission, /message отдаёт сообщение
 *  только после потребления). Мусор стрипится почастино. */
export function inboxToSessionMsgs(entries: unknown[]): SessionMsg[] {
  const out: SessionMsg[] = []
  for (const e of entries) {
    if (e === null || typeof e !== "object") continue
    const x = e as Record<string, unknown>
    if (x.type !== "user") continue
    const payload = x.payload as Record<string, unknown> | undefined
    const text = payload?.text
    const created = (x.time as { created?: unknown } | undefined)?.created
    if (typeof text !== "string" || text.trim() === "" || typeof created !== "number" || !Number.isFinite(created)) continue
    out.push({
      id: typeof x.id === "string" ? x.id : undefined,
      type: "user",
      time: { created },
      text,
    })
  }
  return out
}

// ——— seen/ack: канал исхода разбора (протокол финала демона) ———

/** Префикс машиночитаемой строки исхода в финале хода (методичка agent.md). */
const ACK_LINE_PREFIX = "draft-ack:"

/** Срез markdown-обвеса по краям токена (бэктики/кавычки/жирный/подчёрк):
 *  LLM любит украшать, ids на доске «голые». */
const stripMdEdge = (raw: string): string =>
  raw.replace(/^[`'"*_]+/, "").replace(/[`'"*_]+$/, "").trim()

/** Разбор draft-ack строк финала: `draft-ack: <rowId>=<вердикт> …` — пары
 *  через пробел/запятую/`;` (пробелы вокруг `=` тоже терпимы), строк может
 *  быть несколько, позиция в строке любая. Токен вердикта нормализуется
 *  (обвес/trim/lower), неизвестный вердикт или кривая пара — игнор (не ack →
 *  фолбэк анти-петли). Повтор rowId в разных строках — last-wins (агент
 *  передумал в ходе). */
export function parseDraftAcks(text: string): Record<string, DraftVerdict> {
  const out: Record<string, DraftVerdict> = {}
  for (const line of text.split("\n")) {
    const at = line.indexOf(ACK_LINE_PREFIX)
    if (at === -1) continue
    const segment = line.slice(at + ACK_LINE_PREFIX.length)
    for (const m of segment.matchAll(/([^\s,;=]+)\s*=\s*([^\s,;=]+)/g)) {
      const rawRowId = m[1]
      const rawVerdict = m[2]
      if (rawRowId === undefined || rawVerdict === undefined) continue
      const rowId = stripMdEdge(rawRowId)
      const verdict = DRAFT_VERDICTS.find((v) => v === stripMdEdge(rawVerdict).toLowerCase())
      if (rowId !== "" && verdict !== undefined) out[rowId] = verdict
    }
  }
  return out
}

/** Вырезание draft-ack строк из финала: технический канал демона не светит
 *  владельцу в ТГ-доставке. */
export function stripDraftAcks(text: string): string {
  return text.split("\n").filter((line) => !line.includes(ACK_LINE_PREFIX)).join("\n").trim()
}

// ——— финал хода ———

export interface SessionMsg {
  id?: string
  type?: string
  time?: { created?: number; completed?: number }
  /** user-сообщения: API отдаёт текст верхним уровнем (Session.Message.User
   *  в 2.2.5, content отсутствует) */
  text?: string
  content?: { type?: string; text?: string }[]
}

/** Готовый ответ агента: склейка text-частей одного assistant-сообщения. */
export interface Reply {
  id: string
  text: string
  at: number // ms time.created
}

/** Ответы хода по порядку (ASC): assistant-сообщения новее sinceMs с непустым
 *  текстом, не из delivered (дедуп инкрементальной доставки). completedOnly
 *  отсекает незавершённые (стрим идёт); для хвоста финала — false: оборванный
 *  ход доставляет частичный текст, как до инкрементальной доставки. API отдаёт
 *  DESC — реверсим. */
export function extractReplies(
  msgs: SessionMsg[],
  sinceMs: number,
  delivered: readonly string[] = [],
  completedOnly = true,
): Reply[] {
  const seen = new Set(delivered)
  const out: Reply[] = []
  for (const m of msgs) {
    if (m.type !== "assistant") continue
    if (completedOnly && m.time?.completed === undefined) continue
    const at = m.time?.created ?? 0
    if (at < sinceMs) continue
    if (m.id === undefined || seen.has(m.id)) continue
    const text = textOf(m)
    if (text === "") continue
    out.push({ id: m.id, text, at })
  }
  return out.reverse()
}

/** Текст сообщения сессии: user-сообщения API отдаёт текстом верхнего уровня
 *  (Session.Message.User, 2.2.5 — content у них отсутствует), assistant —
 *  content-частями. Читаем обе формы — promptLandedAt без этого не матчится. */
const textOf = (m: SessionMsg): string => {
  if (typeof m.text === "string" && m.text.trim() !== "") return m.text.trim()
  return (m.content ?? [])
    .filter((p) => p?.type === "text" && typeof p.text === "string")
    .map((p) => p.text as string)
    .join("\n")
    .trim()
}

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
    const chunk = source.slice(pos, end)
    pos = end
    return chunk
  })
}

// ——— профиль и источники промптов ———

/** markdown → TG HTML конверсия (зависимость пакета-входа; ядро lib без
 *  внешних зависимостей — md-to-telegram живёт в node_modules входа). */
export interface TgFormatter {
  /** TG-HTML куски; null = конверсия не удалась (plain-фолбэк доставки). */
  htmlParts(text: string): string[] | null
}

export interface HubModel {
  providerID: string
  id: string
  variant?: string
}

/** Контекст источника промптов: флаг останова + учёт завершения лупа. */
export interface HubSourceCtl {
  stopping(): boolean
  /** луп источника дошёл до безопасной точки — координированный останов */
  done(): void
}

/** Источник промптов помимо ТГ (вотчер черновиков у триажа): кладёт заявки
 *  в общую очередь — единый пайп батчей/стирта. */
export interface HubSource {
  /** ключ loopDone для координированного останова */
  name: string
  start(hub: Hub, ctl: HubSourceCtl): Promise<void>
}

/** Хук финала хода (триаж: draft-ack трекер + доставка «обсуждение»).
 *  acks — разбор машиночитаемых строк финала по ВСЕМ assistant-
 *  текстам хода. unchat не-null для хода без чата (chatID=0): body — финал
 *  после стрипа ack-строк (или «(пустой ответ)») с broken-пометкой; хук
 *  решает доставку (у триажа — вердикт «обсуждение» в залоченный чат). */
export type HubFinalHook = (
  run: RunState,
  acks: Record<string, DraftVerdict>,
  unchat: { body: string; broken: boolean } | null,
  hub: Hub,
) => Promise<void>

export interface HubProfile {
  /** имя демона: state-каталог, лог, startup-строка («uwuwu-triage») */
  daemonName: string
  /** каталог входа: hub.json + agent.md (секреты и деф — не в lib) */
  here: string
  /** имя нативного деф-агента сессии */
  agentName: string
  /** имя файла дефа в here (дефолт agent.md — как у триажа) */
  agentDefFile?: string
  /** тайтл вечной сессии диалога */
  sessionTitle: string
  /** /start справка */
  help: string
  /** env корня рантайм-путей (state/лог/workspace/media); дефолт — homedir */
  homeEnv: string
  /** env TG-API override (mock-TG досмерджевого протокола) */
  tgApiEnv: string
  /** env e2e-маркера сессий («1» → тайтл с префиксом [e2e]); дример
   *  пропускает помеченные сессии (конвенция lib/e2e-tag) */
  e2eEnv?: string
  /** env дебаунса батча, ms */
  batchEnv: string
  /** env тика поллинга, сек */
  pollEnv: string
  /** дефолт модели сессии (формат с variant — паритет dreamer); hub.json
   *  model переопределяет целиком */
  model: HubModel
  /** markdown → TG HTML (инжект входа) */
  tgFormatter: TgFormatter
  /** источники промптов помимо ТГ */
  sources: HubSource[]
  /** хук финала хода (draft-трекер триажа); без хука — generic-поведение */
  onFinal?: HubFinalHook
  /** extra-валидатор state.json входа (черновик-трекер триажа):
   *  битые поля стрипятся внутри хука, база не роняется */
  validateState?: (raw: Record<string, unknown>, out: HubState) => void
  /** старт-сверка демона между syncAgentDef и лупами/источниками (пиннинг
   *  за нодой, ADR-076): throw = отказ старта — демон умирает до любой
   *  поллинг-работы; деф к этому моменту уже материализован */
  beforeLoops?: (hub: Hub) => Promise<void>
  /** порог тишины выходного потока, ms: доставок и завершённых ходов нет
   *  дольше порога при живом процессе → алерт владельцу (silenceLoop).
   *  Атрибут профиля: триаж — сутки (волны черновиков ежедневные) */
  silenceTimeoutMs: number
  /** период тика silenceLoop, ms; дефолт минута. Инъекция для e2e —
   *  прецедент ADR-061 («пейсинг ретраев — инъекция для тестов») */
  silenceTickMs?: number
  /** окно ретрая недоставленного алерта, ms; дефолт SILENCE_ALERT_RETRY_MS.
   *  Инъекция для e2e — паритет silenceTickMs */
  silenceAlertRetryMs?: number
  /** Политика автокомпакта вечной сессии (lib/compact): idle ≥ idleMs и
   *  контекст ≥ minContextPct% лимита модели → нативный POST /compact.
   *  undefined — луп не стартует (safe default, откат = снять поле). */
  compact?: CompactPolicy
}

export interface Cfg {
  token: string
  owner: string
  model?: HubModel
}

/** Публичная поверхность хаба для источников и хуков. */
export interface Hub {
  readonly profile: HubProfile
  /** живое (мутабельное) состояние — источники читают/пишут напрямую */
  readonly state: HubState
  /** конфиг hub.json (заполнен после чтения на старте) */
  readonly cfg: Partial<Cfg>
  save(): void
  log(m: string): void
  /** заявка в общую очередь: push + lastPushAt + save (пайп батчей единый) */
  enqueue(req: Request): void
  /** синтетический msgID не-ТГ источника: отрицательные никогда не
   *  пересекаются с ТГ message_id */
  nextSyntheticMsgID(): number
  send(chatID: number, text: string): Promise<boolean>
  deliverWithRetries(chatID: number, body: string, label: string): Promise<void>
  /** полный старт: конфиг, lock, деф, warning изоляции, петли */
  run(): Promise<void>
}

// ——— фабрика хаба (один процесс = один хаб) ———

export function createHub(profile: HubProfile): Hub {
  const CFG = join(profile.here, "hub.json")
  const AGENT_DEF_SRC = join(profile.here, profile.agentDefFile ?? "agent.md")
  /** Корень рантайм-путей: дефолт — прод (systemd); override — только
   *  досмерджевый протокол (mock-TG E2E рядом с живым демоном не должен
   *  трогать прод-state, прод-воркспейс и прод-лог). */
  const RUN_ROOT = process.env[profile.homeEnv] ?? homedir()
  const STATE_DIR = join(RUN_ROOT, ".local/state", profile.daemonName)
  const STATE_FILE = join(STATE_DIR, "state.json")
  const LOG_PATH = join(RUN_ROOT, ".local/share/opencode/log", `${profile.daemonName}.log`)
  /** изолированный cwd сессии диалога: недоверенный текст, домашний каталог
   *  сессия видеть не должна; деф агента материализуется в .opencode/agents/
   *  отсюда (нативный механизм конфигурации). Деф без ограничений —
   *  осознанное решение владельца; сдерживание недоверенных текстов —
   *  методичка + метаданные промпта + guard CLI. */
  const WORKSPACE = join(RUN_ROOT, ".local/share", profile.daemonName, "workspace")
  const AGENT_DEF_DST = join(WORKSPACE, ".opencode/agents", `${profile.agentName}.md`)
  /** выгруженные вложения заявок (скрины/файлы) */
  const MEDIA_DIR = join(RUN_ROOT, ".local/share", profile.daemonName, "media")
  const MEDIA_TTL_MS = 7 * 24 * 3600_000
  /** Регистрация фонового opencode-сервиса — ВХОД демона (где живой сервер),
   *  не его state: всегда реальный homedir, RUN_ROOT его не двигает. */
  const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
  const TG_API = process.env[profile.tgApiEnv] ?? "https://api.telegram.org"
  const E2E_SESSIONS = profile.e2eEnv !== undefined && process.env[profile.e2eEnv] === "1"

  // NaN/мусор из env → дефолт; hot-loop поллинга при 0 исключаем
  const POLL_SEC = Math.max(1, Number(process.env[profile.pollEnv] ?? 5) || 5)
  const MAX_SPAWN_ATTEMPTS = 5 // dead-letter после N неудачных попыток хода подряд
  const DELIVER_ATTEMPTS = 3 // попытки доставки финала в ТГ
  /** дебаунс батча: поток сообщений склеивается в один ход, юзер получает окно
   *  на редактирование (edited_message подменяет текст, пока батч в очереди) */
  const BATCH_DELAY_MS = Math.max(0, Number(process.env[profile.batchEnv] ?? 60_000) || 60_000)
  const BATCH_MAX_MS = 5 * 60_000 // самое старое сообщение не ждёт дольше
  /** окно чтения истории сессии: `?limit&order=desc` вместо полного списка —
   *  недоставленный хвост всегда в свежайших (между тиками >100 недоставленных
   *  не накопить; delivered[] переживает рестарты), O(history) на тик не растёт */
  const MSG_WINDOW = 100
  /** порция без-чатовых заявок в одном ходе (бэклог не должен деградировать
   *  вечную сессию — контекст/компакция) */
  const UNCHAT_BATCH_MAX = 5

  const log = (m: string): void => {
    try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
  }
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

  // конфиг: ошибки чтения не роняем на импорте — run() скажет человечно
  let cfg: Partial<Cfg> = {}
  try { cfg = JSON.parse(readFileSync(CFG, "utf-8")) as Partial<Cfg> } catch {}

  // state: терпеливое чтение (битый файл не должен крашить always-on демон)
  // + атомарная запись (tmp+rename) с бэкапом
  let state: HubState = { offset: 0, queue: [] }
  if (existsSync(STATE_FILE)) {
    try { state = validState(JSON.parse(readFileSync(STATE_FILE, "utf-8")), profile.validateState) }
    catch (e) { log(`state.json битый — стартую с пустого (${e})`) }
  }
  // материализация метки тишины — мутатор, а не валидатор (паттерн инцидента
  // draftLoop: опц-поле материализует место мутации, контракт валидатора
  // «стрипать битое» остаётся честным). Апгрейд живого state без ложного
  // алерта: метка стартует с момента первого запуска нового кода.
  state.lastDeliveryAt ??= Date.now()
  const save = (): void => {
    mkdirSync(STATE_DIR, { recursive: true })
    if (existsSync(STATE_FILE)) {
      try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
    }
    const tmp = `${STATE_FILE}.tmp`
    writeFileSync(tmp, JSON.stringify(state, null, 1))
    renameSync(tmp, STATE_FILE)
  }

  // координированный останов: все циклы доходят до безопасной точки, процесс
  // выходит сам; grace-таймаут в acquireLock — страховка от зависшего drainage
  let stopping = false
  const loopDone: Record<string, boolean> = { run: false, poll: false, silence: false }
  for (const s of profile.sources) loopDone[s.name] = false
  const maybeExit = (): void => {
    if (Object.values(loopDone).every((v) => v)) {
      save()
      process.exit(0)
    }
  }

  // ——— Telegram ———

  /** ТГ: дефолт — прод-API; override для mock-TG (досмерджевый протокол). */
  async function tg(method: string, params: Record<string, unknown> = {}, timeoutMs = 30_000): Promise<unknown> {
    const res = await fetch(`${TG_API}/bot${cfg.token}/${method}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(params),
      signal: AbortSignal.timeout(timeoutMs),
    })
    const json = await res.json() as { ok?: boolean; description?: string; parameters?: { retry_after?: number }; result?: unknown }
    if (json.ok !== true) {
      // parameters (retry_after при 429) пробрасываем наверх для ретрая
      throw Object.assign(new Error(`TG ${method}: ${json.description ?? "?"}`), { parameters: json.parameters })
    }
    return json.result
  }

  /** Одна попытка отправки: 429 уважаем retry_after (один ретрай). */
  const attemptSend = async (chatID: number, part: string, html: boolean): Promise<boolean> => {
    const post = () => tg("sendMessage", { chat_id: chatID, text: part, ...(html ? { parse_mode: "HTML" } : {}) })
    try {
      await post()
      return true
    } catch (e) {
      const wait = Number((e as { parameters?: { retry_after?: number } }).parameters?.retry_after ?? 0)
      if (wait > 0) {
        try { await sleep((wait + 1) * 1000); await post(); return true } catch {}
      }
      log(`send: ${e}`)
      return false
    }
  }

  /** plain-доставка: нарезка исходника под лимит ТГ. */
  const sendPlain = async (chatID: number, text: string): Promise<boolean> => {
    for (const c of plainChunks(text)) {
      if (!(await attemptSend(chatID, c, false))) return false
    }
    return true
  }

  /** markdown агента → TG HTML (AST-конверсия, инжект входа); упавшая
   *  html-часть и всё после неё доезжает исходным текстом (куски выровнены) —
   *  уже отправленное не дублируется, теги в чат не утекают. Ошибки глотает:
   *  false = не доставлено (никогда не реджектит — вызывающие ветки сами
   *  решают, что делать). */
  const sendRaw = async (chatID: number, text: string): Promise<boolean> => {
    const html = profile.tgFormatter.htmlParts(text) ?? []
    if (html.length === 0) return sendPlain(chatID, text)
    const aligned = alignChunks(html, text)
    for (const [i, part] of html.entries()) {
      if (await attemptSend(chatID, part, true)) continue
      for (const tail of aligned.slice(i)) {
        if (!(await attemptSend(chatID, tail, false))) return false
      }
      return true
    }
    return true
  }

  /** Доставка в ТГ + учёт выхода тишины: успех = выходной поток жив, метка
   *  обновлена; неудача метку не трогает — тишина продолжает расти и задетектится
   *  silenceLoop (в т.ч. ретрай недоставленного алерта по своему окну). */
  const send = async (chatID: number, text: string): Promise<boolean> => {
    const ok = await sendRaw(chatID, text)
    if (ok) state.lastDeliveryAt = Date.now()
    return ok
  }

  // ——— opencode REST (registration-файл фонового сервиса; перечитываем на
  //  каждый вызов — файл меняется при рестарте сервиса, кэш = stale url) ———

  async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
    const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
    const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
    const auth = `Basic ${Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")}`
    const res = await fetch(`${base}/api${path}`, {
      method: init?.method ?? "GET",
      headers: { authorization: auth, "content-type": "application/json" },
      body: init?.body === undefined ? undefined : JSON.stringify(init.body),
      signal: AbortSignal.timeout(15_000),
    })
    const text = await res.text()
    if (!res.ok) throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
    if (text.trim() === "") return {}
    const j = JSON.parse(text) as { data?: unknown }
    return j.data ?? j
  }

  async function sessionMessages(sessionID: string): Promise<SessionMsg[]> {
    const msgs = await restCall(`/session/${sessionID}/message?limit=${MSG_WINDOW}&order=desc`)
    return Array.isArray(msgs) ? msgs as SessionMsg[] : []
  }

  /** Признанные (admitted) входы сессии: стирты между admission и потреблением
   *  агент-лупом в /message ещё не видны — рестарт в этом окне без inbox-чека
   *  пере-POST-ил бы уже приземлённый стирт. Сбой чтения — дедуп живёт по
   *  /message (краш-окно сузится, но не сломается). */
  async function sessionInbox(sessionID: string): Promise<SessionMsg[]> {
    try {
      const entries = await restCall(`/session/${sessionID}/inbox`)
      return inboxToSessionMsgs(Array.isArray(entries) ? entries as unknown[] : [])
    } catch (e) {
      log(`inbox ${sessionID} не достали (${e}) — дедуп по /message`)
      return []
    }
  }

  /** Дедуп стирта: приземление ищем и в /message (потреблённые), и в inbox
   *  (признанные, ещё не потреблённые). */
  const steerLandedAt = async (run: RunState, msgs: SessionMsg[], sinceMs: number, text: string): Promise<number | null> =>
    promptLandedAt(msgs, sinceMs, text) ?? promptLandedAt(await sessionInbox(run.sessionID), sinceMs, text)

  // ——— вечная сессия диалога ———

  /** Материализовать нативный деф агента в воркспейс (если изменился) —
   *  сессии, созданные в этом location с agent:<agentName>, получают его
   *  системным промптом + нативными правами. Вызов и на старте демона, и
   *  перед созданием сессии: правка agent.md в репо подхватывается без
   *  рестарта (живая сессия — нативным switchAgent в ensureSession).
   *  Атомарно (tmp+rename). Заодно сносит stale-AGENTS.md прошлой схемы. */
  const syncAgentDef = (): void => {
    mkdirSync(dirname(AGENT_DEF_DST), { recursive: true })
    const src = readFileSync(AGENT_DEF_SRC, "utf-8")
    let prev = ""
    try { prev = readFileSync(AGENT_DEF_DST, "utf-8") } catch {}
    if (prev !== src) {
      const tmp = `${AGENT_DEF_DST}.tmp`
      writeFileSync(tmp, src)
      renameSync(tmp, AGENT_DEF_DST)
      log(`деф агента ${profile.agentName} материализован (${src.length} байт)`)
    }
    const staleAgents = join(WORKSPACE, "AGENTS.md")
    if (existsSync(staleAgents)) {
      try { rmSync(staleAgents) } catch {}
      log("stale AGENTS.md воркспейса удалён (схема до нативных дефов)")
    }
  }

  /** Жива ли сессия (404/400 = нет). null = неизвестно, не блокируем. */
  const sessionAlive = async (sessionID: string): Promise<boolean | null> => {
    try {
      await restCall(`/session/${sessionID}`)
      return true
    } catch (e) {
      const status = (e as { status?: number }).status
      if (status === 404 || status === 400) return false
      return null
    }
  }

  /** Модель сессии: hub.json переопределяет, фолбэк — дефолт профиля
   *  (формат с variant; NB: model из дефа не применяется к primary-сессиям —
   *  передаём явно при создании и switchModel). */
  const sessionModel = (): HubModel => cfg.model ?? profile.model

  /** Единая сессия диалога: создаётся один раз, переживает рестарты демона
   *  (контекст всей переписки + компакция); пропала (снесли в TUI) — пересоздаём,
   *  контекст теряется: лог + честное уведомление юзеру.
   *  Деф горячий (эмпирика 2.2.5, чистые сессии без отравленной истории):
   *  сервер перечитывает agent-def на каждом промпте — правка agent.md в репо
   *  доезжает живой сессии следующим же сообщением (демон материализует деф в
   *  ensureSession). Гочи проб: не гонять видимость правил в сессии, где модель
   *  уже отрицала их — история диалога сильнее системного обновления. */
  const ensureSession = async (): Promise<string> => {
    if (state.session !== undefined) {
      const alive = await sessionAlive(state.session)
      if (alive === true) {
        syncAgentDef() // правка дефа в репо → живая сессия следующим сообщением
        try { await restCall(`/session/${state.session}/model`, { method: "POST", body: { model: sessionModel() } }) } catch (e) { log(`switchModel: ${e}`) }
        return state.session
      }
      if (alive === null) return state.session // неизвестно — не пересоздаём
      log(`сессия ${state.session} пропала — пересоздаю (контекст переписки потерян)`)
      return await recreateSession(true)
    }
    return await recreateSession(false)
  }

  const recreateSession = async (notify: boolean): Promise<string> => {
    syncAgentDef()
    const raw = await restCall("/session", {
      method: "POST",
      body: {
        title: E2E_SESSIONS ? withE2ETag(profile.sessionTitle) : profile.sessionTitle,
        agent: profile.agentName,
        // NB: model из дефа не применяется к primary-сессиям — передаём явно
        model: sessionModel(),
        location: { directory: WORKSPACE },
      },
    })
    const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
    if (typeof id !== "string" || !id.startsWith("ses_")) throw new Error("сессия не поднялась")
    state.session = id
    save()
    log(`сессия диалога: ${id}`)
    if (notify && state.chatID !== undefined) {
      await send(state.chatID, "ℹ️ Сессия диалога пересоздана — контекст переписки сброшен.")
    }
    return id
  }

  /** Неудача попытки хода: transient или poison. Счётчик ведём на
   *  ПРЕДСТАВИТЕЛЕ батча в очереди (состав не трогаем — склейка пересоберётся
   *  теми же сообщениями, дублей не будет); MAX_SPAWN_ATTEMPTS — dead-letter:
   *  весь батч (dropBatch) снимаем и персистим ДО отправки, юзеру уходит
   *  полный склеенный текст; недоставка — текст в лог. */
  async function spawnFail(req: Request, e: unknown): Promise<void> {
    const rep = state.queue.find((r) => r.msgID === req.msgID)
    if (rep === undefined) {
      log(`spawnFail: заявка msg${req.msgID} уже не в очереди — игнор`)
      return
    }
    const attempts = (rep.attempts ?? 0) + 1
    if (attempts >= MAX_SPAWN_ATTEMPTS) {
      state.queue = dropBatch(state.queue, req)
      state.firing = undefined
      save()
      // текст заявки — в лог ДО отправки: крах посреди send не потеряет его молча
      log(`заявка msg${req.msgID}: ${attempts} неудачных попыток хода (${e}) — dead-letter, текст: ${req.text}`)
      // dead-letter — единственное ТГ-уведомление по без-чатовым входам
      // (иначе застрявший ход — невидимая авария): по chatID=0 шлём в залоченный чат
      const notifyID = req.chatID !== 0 ? req.chatID : state.chatID
      const ok = notifyID !== undefined
        && await send(notifyID, `⚠️ Не смог поднять ход ${attempts} раз — заявка снята с обработки, продублируй позже:\n\n${req.text}`)
      if (!ok) log(`dead-letter msg${req.msgID} НЕ доставлен (текст заявки выше в этом логе)`)
      return
    }
    rep.attempts = attempts // счётчик на представителе: состав батча в очереди не трогаем
    state.firing = undefined // полёт не состоялся, батч в очереди — правки снова можно
    save()
    log(`ход не удался (${e}) — ретрай ${attempts}/${MAX_SPAWN_ATTEMPTS - 1} через 30с`)
    if (req.chatID !== 0) {
      await send(req.chatID, `⚠️ Прогон не поднялся (попытка ${attempts} из ${MAX_SPAWN_ATTEMPTS - 1}: ${e instanceof Error ? e.message : String(e)}) — ретрай через 30с`)
    }
    await sleep(30_000)
  }

  /** Доставка текста в ТГ с ретраями: ТГ лежит дольше всех попыток — ход уже
   *  отработал, теряем только ответ: фиксируем полный текст в лог. */
  const deliverWithRetries = async (chatID: number, body: string, label: string): Promise<void> => {
    for (let a = 1; a <= DELIVER_ATTEMPTS; a++) {
      if (await send(chatID, body)) {
        log(`${label} (доставлен, попытка ${a}): ${body.slice(0, 80)}`)
        return
      }
      log(`${label}: попытка ${a}/${DELIVER_ATTEMPTS} не прошла`)
      if (a < DELIVER_ATTEMPTS) await sleep(30_000)
    }
    log(`${label} НЕ доставлен (ТГ недоступен?) — текст: ${body}`)
  }

  /** Недоставленные готовые ответы хода — в ТГ (инкрементальная доставка,
   *  каждый тик watchRun; финальный хвост доставит deliverFinal с ретраями).
   *  Хронология: стоп на первой неудачной отправке — недоставленное ретраится
   *  следующим тиком, поздние ответы его не обгоняют. Промежуточные сообщения
   *  из одних draft-ack строк после стрипа пусты — помечаются доставленными
   *  без отправки (ack-канал демона, владельцу не светит). */
  const deliverNew = async (run: RunState, msgs: SessionMsg[]): Promise<void> => {
    if (run.req.chatID === 0) return // ход без чата: доставка только в финале (хук)
    const replies = extractReplies(msgs, run.startedAt, run.delivered ?? [], true)
    let changed = false
    for (const r of replies) {
      if (state.run !== run) return // /reset в окне await — state не трогаем
      const body = stripDraftAcks(r.text)
      if (body === "") {
        run.delivered = [...(run.delivered ?? []), r.id]
        changed = true
        log(`ход ${run.sessionID}: ${r.id} — только draft-ack, в ТГ не шлю`)
        continue
      }
      const ok = await send(run.req.chatID, body)
      if (state.run !== run) return
      if (!ok) {
        log(`ход ${run.sessionID}: ${r.id} не доставлен — стоп до следующего тика (хронология)`)
        return
      }
      run.delivered = [...(run.delivered ?? []), r.id]
      changed = true
      log(`ход ${run.sessionID}: ответ ${r.id} доставлен посреди хода: ${body.slice(0, 60)}`)
    }
    if (changed && state.run === run) save()
  }

  /** POST стирта с ретраем без files (паттерн startRun). firing-маркер —
   *  СИНХРОННО до await (edit-ветка честно скажет «уже у агента»; персона
   *  батча персистится для краш-восстановления — симметрично run.req старта).
   *  Успех: граница done хода (run.steerAt) = time.created ответа — сервер
   *  всю фазу исполнения, начатого стиртом по idle-сессии, отдаёт стухший
   *  outcome/idle (проба Design); не число — Date.now() (та же машина).
   *  Неуспех: маркер долой, батч остаётся в очереди — ретрай следующим тиком
   *  (счётчик не нужен: 404 закроет gone-ветка, трансзиент самолечится). */
  const steerPost = async (run: RunState, batch: Request): Promise<void> => {
    if (state.run !== run) return
    state.firing = { at: Date.now(), msgIDs: batch.msgIDs ?? [batch.msgID] }
    save()
    const files = (batch.attachments ?? []).map((a) => ({ uri: `file://${a.path}`, name: a.name }))
    const post = (withFiles: boolean) => restCall(`/session/${run.sessionID}/prompt`, {
      method: "POST",
      body: { text: batch.text, delivery: "steer", ...(withFiles && files.length > 0 ? { files } : {}) },
    })
    let landed: unknown
    try {
      try {
        landed = await post(true)
      } catch (e) {
        if (files.length === 0) throw e
        log(`стирт с files не прошёл (${e}) — ретрай без files`)
        landed = await post(false)
      }
    } catch (e) {
      if (state.run === run) {
        state.firing = undefined
        save()
      }
      log(`стирт не прошёл (${e}) — батч в очереди, ретрай следующим тиком`)
      return
    }
    if (state.run !== run) return // /reset в окне POST — батч остаётся в очереди, поднимется новым ходом
    const t = (landed as { time?: { created?: unknown } } | undefined)?.time?.created
    run.steerAt = typeof t === "number" && Number.isFinite(t) && t > 0 ? t : Date.now()
    state.queue = dropBatch(state.queue, batch)
    state.firing = undefined
    save()
    log(`стирт ${run.sessionID} (${batch.msgIDs?.length ?? 1} сообщ., граница done ${run.steerAt}): ${batch.text.slice(0, 60)}`)
  }

  /** Вброс ТГ-батча в активный ход. Порядок проверок — контракт (ревью
   *  Common#2): краш-восстановление по персистентному firing ПЕРВИЧНО (точная
   *  персона стиртованного батча из msgIDs; пересборка по готовности могла бы
   *  прицепить сообщения, пришедшие до рестарта), затем обычный кандидат по
   *  дебаунсу; landed-дедуп раньше всякого POST. Граница дедупа — момент
   *  готовности/полёта батча (firing.at / readyAt, минус 2с скью), НЕ startedAt:
   *  повтор юзером того же текста внутри хода дедупом не ловится. */
  const steerStep = async (run: RunState, msgs: SessionMsg[]): Promise<void> => {
    if (run.req.chatID === 0) return // ход без чата — вброс не критичен

    if (state.firing !== undefined) {
      // маркер при активном ходе — всегда след стирта прошлой жизни демона:
      // старт-маркер к этому моменту вычищен (дозрел до run и сброшен в
      // startRun, либо снесён вместе с run startedAt=0 в runLoop)
      const batch = steerRecoveryBatch(state.queue, state.firing.msgIDs)
      if (batch === null) {
        log(`стирт: firing-маркер без персоны в очереди — чищу`)
        state.firing = undefined
        save()
        return
      }
      const landed = await steerLandedAt(run, msgs, state.firing.at - 2000, batch.text)
      if (landed !== null) {
        // POST прошлой жизни долетел, save не пережил: дроп ТОЧНОЙ персоны,
        // граница done восстанавливается из момента приземления
        if (state.run === run) {
          run.steerAt = landed
          state.queue = dropBatch(state.queue, batch)
          state.firing = undefined
          save()
          log(`стирт (краш-окно): приземлился ${landed} — батч дропнут, наблюдение продолжается`)
        }
        return
      }
      log(`стирт (краш-окно): маркер без приземления — ретрай той же персоной`)
      await steerPost(run, batch)
      return
    }

    const cand = steerCandidate(state.queue, state.lastPushAt ?? 0, BATCH_DELAY_MS, BATCH_MAX_MS)
    if (cand === null || Date.now() < cand.readyAt) return
    const landed = await steerLandedAt(run, msgs, cand.readyAt - 2000, cand.batch.text)
    if (landed !== null) {
      // дедуп рестарт-окна без маркера (страховка; в живом процессе приземление
      // без firing невозможен — маркер персистится ДО POST)
      if (state.run === run) {
        run.steerAt = landed
        state.queue = dropBatch(state.queue, cand.batch)
        save()
        log(`стирт: батч уже в сессии (${landed}) — дроп без повторного POST`)
      }
      return
    }
    await steerPost(run, cand.batch)
  }

  /** Финал хода: хвост недоставленных ответов — обязательный канал (ретраи);
   *  acks парсятся по ВСЕМ assistant-текстам хода (промежуточные сообщения
   *  переживают инкрементальную доставку и рестарты). Оборванный финал
   *  (outcome ≠ succeeded) — пометка юзеру: ход завершён, сообщение не
   *  переигрывается и сессия не пересоздаётся (одна вечная сессия; сброс
   *  контекста — только ручной /reset). allowSilent (крон-промпты): ход без
   *  единого ответа — молчание, не «(пустой ответ)»; оборванный — пометка. */
  const deliverFinal = async (run: RunState): Promise<void> => {
    // финал хода = выходная запись тишины при любом исходе (включая unchat-
    // финалы черновиков и тихие ходы): ход завершился — пайп жив и отработал;
    // недоставленность ответов ловит send-метка (не обновится при провале)
    state.lastDeliveryAt = Date.now()
    let broken = false
    try {
      const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string }
      broken = s.outcome !== undefined && s.outcome !== "succeeded"
    } catch { /* статус не достали — попробуем всё равно прочитать сообщения */ }
    let msgs: SessionMsg[] = []
    try { msgs = await sessionMessages(run.sessionID) } catch {}
    const repliesAll = extractReplies(msgs, run.startedAt, [], false)
    const acks = parseDraftAcks(repliesAll.map((r) => r.text).join("\n"))
    const BROKEN_MARK = "\n\n⚠️ прогон оборван — финал мог не доехать"
    // ход без чата (chatID=0): финал — в лог (доска/пайп — источник истины),
    // доставку решает хук (триаж: вердикт «обсуждение» в залоченный чат)
    let unchat: { body: string; broken: boolean } | null = null
    if (run.req.chatID === 0) {
      const last = repliesAll.at(-1)
      const text = last?.text ?? ""
      const body = (text !== "" ? stripDraftAcks(text) : "(пустой ответ)") + (broken ? BROKEN_MARK : "")
      log(`финал ${run.sessionID} (без чата): ${(text !== "" ? text : "(пустой ответ)").slice(0, 200)}`)
      unchat = { body, broken }
    } else {
      const tail = extractReplies(msgs, run.startedAt, run.delivered ?? [], false)
        .map((r: Reply) => stripDraftAcks(r.text))
        .filter((body) => body !== "")
      const deliveredAny = (run.delivered ?? []).length > 0
      if (tail.length === 0 && !deliveredAny) {
        if (run.req.allowSilent === true) {
          // тихий ход (молчание — легитимный исход крон-промпта); оборванный —
          // пометка: сводки не было, но прогон упал — владелец должен знать
          if (broken) {
            await deliverWithRetries(run.req.chatID, `⚠️ прогон оборван — финал мог не доехать`, `финал ${run.sessionID} (пометка)`)
          } else {
            log(`финал ${run.sessionID}: тихий ход — молчание (allowSilent)`)
          }
        } else {
          // за весь ход не доставлено ничего и хвоста нет — честный пустой ответ
          await deliverWithRetries(run.req.chatID, `(пустой ответ)${broken ? BROKEN_MARK : ""}`, `финал ${run.sessionID}`)
        }
      } else {
        for (const [i, part] of tail.entries()) {
          const body = broken && i === tail.length - 1 ? part + BROKEN_MARK : part
          await deliverWithRetries(run.req.chatID, body, `хвост ${run.sessionID} (${i + 1}/${tail.length})`)
        }
        if (broken && tail.length === 0) {
          // всё доставлено посреди хода, но прогон оборван — пометка отдельно
          await deliverWithRetries(run.req.chatID, `⚠️ прогон оборван — финал мог не доехать`, `финал ${run.sessionID} (пометка)`)
        }
      }
    }
    // draft-трекер/«обсуждение» — хук профиля, ОДИН вызов на финал (триаж:
    // logAckParse + recordDraftAcks + applyTurns при любом исходе хода и
    // доставка unchat; без хука — generic-поведение)
    await profile.onFinal?.(run, acks, unchat, hub)
  }

  /** Одна проверка сессии: ещё исполняется / завершена / пропала. Граница
   *  «done» — idle новее старта хода И последнего приземлившегося стирта:
   *  исполнение, начатое стиртом по idle-сессии, отдает стухший outcome и
   *  старый idle до своего завершения (проба Design). */
  const sessionStatus = async (run: RunState): Promise<"running" | "done" | "gone"> => {
    try {
      const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string; time?: { idle?: number } }
      const boundary = Math.max(run.startedAt, run.steerAt ?? 0)
      if (s.outcome !== undefined && typeof s.time?.idle === "number" && s.time.idle >= boundary) return "done"
      return "running"
    } catch (e) {
      const status = (e as { status?: number }).status
      return status === 404 || status === 400 ? "gone" : "running" // неизвестно — не блокируем
    }
  }

  /** Дождаться финала хода (поллинг состояния сессии — как reconcile хаба):
   *  outcome выставлен и idle новее границы (старт/стирт) → ход завершён.
   *  Таймаута нет: ход живёт столько, сколько живёт; стоп-кран один и ручной —
   *  /reset (чистит state.run → эта петля обязана выйти, не блокируя runLoop).
   *  Каждый тик: один GET сообщений (окно MSG_WINDOW) → инкрементальная
   *  доставка готовых ответов → попытка стирта → статус сессии. */
  const watchRun = async (run: RunState): Promise<"done" | "stopping"> => {
    while (true) {
      if (stopping) return "stopping"
      if (state.run !== run) {
        // ход снесли (/reset): финал не доставляем, очередь свободна
        log(`ход ${run.sessionID}: сброшен (/reset) — перестаю наблюдать`)
        return "done"
      }
      let msgs: SessionMsg[] = []
      try {
        msgs = await sessionMessages(run.sessionID)
      } catch (e) {
        log(`ход ${run.sessionID}: сообщения не достали (${e}) — тик без доставки`)
      }
      await deliverNew(run, msgs)
      await steerStep(run, msgs)
      if (state.run !== run) {
        log(`ход ${run.sessionID}: сброшен (/reset) посреди тика — перестаю наблюдать`)
        return "done"
      }
      const st = await sessionStatus(run)
      if (st === "gone") {
        if (state.run === run) {
          if (run.req.chatID !== 0) {
            await send(run.req.chatID, "⚠️ сессия диалога пропала — сообщение не разобрано, продублируй")
          } else {
            log(`сессия ${run.sessionID} пропала — заявка вернётся в очередь при рестарте`)
          }
        }
        log(`ход ${run.sessionID}: сессия пропала`)
        return "done"
      }
      if (st === "done") {
        if (state.run === run) await deliverFinal(run) // критическая секция: stopping не проверяем — доезжаем
        return "done"
      }
      await sleep(POLL_SEC * 1000)
    }
  }

  /** Рестарт в окне отправки: ход персистнут, батч ещё в очереди — промпт
   *  мог дойти или нет. Дошёл (user-сообщение с ЭТИМ текстом новее старта
   *  хода) → доигрываем с точным startedAt и забираем батч из очереди;
   *  нет → ход сносим, сообщения в очереди поднимутся заново. */
  const confirmPendingRun = async (run: RunState): Promise<boolean> => {
    const ids = run.req.msgIDs ?? [run.req.msgID]
    const inQueue = state.queue.some((r) => ids.includes(r.msgID))
    if (!inQueue) return false // ход подтверждён (батч ушёл из очереди) — наблюдаем
    let landed: number | null = null
    try { landed = promptLandedAt(await sessionMessages(run.sessionID), run.startedAt - 2000, run.req.text) } catch {}
    if (state.run !== run) return true // /reset снёс ход в окне await — не трогаем очередь
    if (landed !== null) {
      log(`окно отправки ${run.sessionID}: промпт дошёл (${landed}) — доигрываем`)
      state.queue = dropBatch(state.queue, run.req)
      state.run = { ...run, startedAt: landed }
      save()
      return false
    }
    log(`окно отправки ${run.sessionID}: промпт не дошёл — ход снесён, сообщения перезапустятся`)
    state.run = undefined
    state.firing = undefined // полёт не состоялся — edit-ветка снова честно примет правки
    save()
    return true // продолжить цикл (батч поднимется заново)
  }

  /** Поднять ход: головной батч из очереди → активный ход в вечной сессии.
   *  Батч НЕ удаляется из очереди до подтверждённого промпта — рестарт в
   *  любой точке окна не теряет его. "idle" — очередь пуста/не готова;
   *  "started" — ход активен; "later" — сбой обработан (sleep в spawnFail). */
  const startRun = async (): Promise<"idle" | "started" | "later"> => {
    const readyAt = batchReadyAt(state.queue, state.lastPushAt ?? 0, BATCH_DELAY_MS, BATCH_MAX_MS)
    if (readyAt === null) return "idle"
    const items = takeBatch(state.queue, readyAt)
    if (items.length === 0) return "idle"
    const batch = capUnchatBatch(items, UNCHAT_BATCH_MAX)
    const req = joinBatch(batch)
    // маркер полёта СИНХРОННО, до любых await: edit-ветка видит «уже пошло»
    // (состав батча в очереди не трогаем — он страховка от потери)
    state.firing = { at: Date.now(), msgIDs: req.msgIDs ?? [req.msgID] }
    save()
    let sessionID: string
    try {
      sessionID = await ensureSession()
    } catch (e) {
      await spawnFail(req, e)
      return "later"
    }
    // /reset в окне ensureSession мог снести state.session — узкий тип врёт,
    // runtime-проверка обязательна (иначе ход упадет на /session/undefined/prompt)
    if (typeof sessionID !== "string" || !sessionID.startsWith("ses_")) return "later"
    // startedAt персистним ДО отправки: крах в мс-окне вокруг POST разрулит
    // confirmPendingRun по тексту (дошёл/не дошёл) — без ложных пустых финалов
    const startedAt = Date.now()
    state.run = { sessionID, startedAt, req }
    save()
    // files-вложение — вижн для мультимодальной модели (путь уже в тексте —
    // даже без вижна агент приложит файл к задаче); не поддерживается моделью
    // → ретрай без файлов, текст несёт пути
    const files = (req.attachments ?? []).map((a) => ({ uri: `file://${a.path}`, name: a.name }))
    const post = (withFiles: boolean) => restCall(`/session/${sessionID}/prompt`, {
      method: "POST",
      body: { text: req.text, ...(withFiles && files.length > 0 ? { files } : {}) },
    })
    try {
      try {
        await post(true)
      } catch (e) {
        if (files.length === 0) throw e
        log(`prompt с files не прошёл (${e}) — ретрай без files`)
        await post(false)
      }
    } catch (e) {
      state.run = undefined
      state.firing = undefined
      save()
      await spawnFail(req, e)
      return "later"
    }
    // промпт подтверждён: батч принят в работу — убираем из очереди целиком
    state.queue = dropBatch(state.queue, req)
    state.firing = undefined
    const waited = Math.round((Date.now() - req.receivedAt) / 1000)
    log(`ход ${sessionID} (${batch.length} сообщ.): ${req.text.slice(0, 60)}${waited > 5 ? ` (ждало в очереди ${waited}с)` : ""}`)
    save()
    return "started"
  }

  const runLoop = async (): Promise<void> => {
    while (true) {
      try {
        if (stopping) break

        if (state.run !== undefined) {
          // рестарт в окне отправки (startedAt=0 — даже не пытались): сносим,
          // сообщение в очереди поднимется само; заодно стухший firing-маркер
          if (state.run.startedAt === 0) {
            state.run = undefined
            state.firing = undefined
            save()
            continue
          }
          // сообщение ещё в очереди → окно отправки: дошёл/не дошёл
          if (await confirmPendingRun(state.run)) continue
        }

        if (state.run !== undefined) {
          if (await watchRun(state.run) === "done") {
            state.run = undefined
            save()
          }
          continue
        }

        // стухший firing-маркер (крах до персиста run): батч цел в очереди
        // и поднимется заново — edit-ветка не должна вечно говорить «в полёте»
        if (state.firing !== undefined && state.run === undefined && Date.now() - state.firing.at > 120_000) {
          state.firing = undefined
          save()
        }

        // дебаунс батча: минута тишины после последнего сообщения (поток
        // склеится, у юзера окно на редактирование); кап — см. batchReadyAt
        const readyAt = batchReadyAt(state.queue, state.lastPushAt ?? 0, BATCH_DELAY_MS, BATCH_MAX_MS)
        if (readyAt !== null && Date.now() < readyAt) {
          await sleep(Math.min(readyAt - Date.now(), 30_000))
          continue
        }

        const r = await startRun()
        if (r === "idle") await sleep(2000)
      } catch (e) {
        log(`runLoop: ${e}`)
        await sleep(5000)
      }
    }
    loopDone.run = true
    maybeExit()
  }

  // ——— вложения: ТГ-медиа → ФС (паттерн зоопарка: getFile → download) ———

  /** Выгрузить медиа из сообщения в MEDIA_DIR (состав и имена — mediaFiles:
   *  photo/document/video/animation/video_note/audio). Возвращает вложения
   *  и маркеры неудач (текст заявки честно скажет агенту, что не выгрузилось). */
  const downloadAttachment = async (m: NonNullable<TgUpdate["message"]>): Promise<{ attachments: Attachment[]; failed: string[] }> => {
    const out: { attachments: Attachment[]; failed: string[] } = { attachments: [], failed: [] }
    const saveFile = async (fileId: string, name: string): Promise<void> => {
      try {
        const f = await tg("getFile", { file_id: fileId }) as { file_path?: string }
        if (typeof f.file_path !== "string") throw new Error("нет file_path")
        const res = await fetch(`${TG_API}/file/bot${cfg.token}/${f.file_path}`, {
          signal: AbortSignal.timeout(30_000),
        })
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        mkdirSync(MEDIA_DIR, { recursive: true })
        const local = join(MEDIA_DIR, name)
        writeFileSync(local, Buffer.from(await res.arrayBuffer()))
        out.attachments.push({ path: local, name })
      } catch (e) {
        log(`вложение ${name}: ${e}`)
        out.failed.push(name)
      }
    }
    for (const { fileId, name } of mediaFiles(m)) await saveFile(fileId, name)
    return out
  }

  /** Гигиена вложений: старше TTL — сносим (заявки живут минутами, неделя — запас). */
  const gcMedia = (): void => {
    try {
      const entries = readdirSync(MEDIA_DIR)
      const now = Date.now()
      for (const e of entries) {
        const p = join(MEDIA_DIR, e)
        try {
          if (now - statSync(p).mtimeMs > MEDIA_TTL_MS) rmSync(p)
        } catch {}
      }
      if (entries.length > 0) log(`gc media: ${entries.length} файлов в каталоге`)
    } catch { /* каталога нет — ок */ }
  }

  // ——— poller ТГ ———

  /** Разбор одного апдейта: offset, привязка чата, диспетчеризация. */
  const handleUpdate = async (u: TgUpdate): Promise<void> => {
    state.offset = u.update_id + 1
    const out = classifyUpdate(u, cfg.owner ?? "")
    if (out.kind === "skip") {
      save()
      return
    }
    // привязка чата: первый личный чат владельца; смена — только вручную
    // через сброс state.json (осознанное решение, не авто)
    if (state.chatID === undefined) {
      state.chatID = out.msg.chat.id
      log(`chat locked: ${state.chatID}`)
    } else if (state.chatID !== out.msg.chat.id) {
      log(`чужой чат ${out.msg.chat.id} — skip`)
      save()
      return
    }
    if (out.kind === "help") {
      await send(state.chatID, profile.help)
    } else if (out.kind === "reset") {
      // стоп-кран: работает ВСЕГДА, включая висящий ход (очередь при этом
      // сохраняется — сообщения поднимутся в новой сессии)
      const wasRunning = state.run !== undefined
      state.run = undefined
      state.firing = undefined
      state.session = undefined
      save()
      log(`сессия диалога сброшена /reset${wasRunning ? " (сброшен и активный ход)" : ""}`)
      await send(state.chatID, "♻️ Сессия сброшена — следующее сообщение начнёт диалог с чистого контекста (очередь сообщений сохранена).")
    } else if (out.kind === "media") {
      await send(state.chatID, "⚠️ Такой тип сообщения не разбираю (голосовые, стикеры и пр.) — опиши словами или пришли фото/файл/видео")
    } else if (out.kind === "edit") {
      // правка в окне дебаунса: подменяем текст, пока батч не ушёл агенту
      if (state.firing?.msgIDs.includes(out.msg.message_id) ?? false) {
        log(`edit msg${out.msg.message_id}: батч уже в полёте — правка не принята`)
        await send(state.chatID, "⏳ Успел раньше меня — сообщение уже у агента. Пришли правку новым сообщением.")
      } else {
        const i = state.queue.findIndex((r) => r.msgID === out.msg.message_id)
        const cur = i >= 0 ? state.queue[i] : undefined
        if (cur !== undefined) {
          // правка может нести НОВОЕ вложение (заменил фото) — выгружаем; текст
          // пересобираем с маркерами (пути в ФС — агент фиксирует их в теле задачи)
          const hasNewMedia = mediaFiles(out.msg).length > 0
          const dl = hasNewMedia ? await downloadAttachment(out.msg) : { attachments: [], failed: [] }
          const attachments = hasNewMedia ? dl.attachments : (cur.attachments ?? [])
          const failed = hasNewMedia ? dl.failed : (cur.failed ?? [])
          state.queue[i] = {
            ...cur,
            text: composeText(out.text, attachments, failed),
            ...(attachments.length > 0 ? { attachments } : {}),
            ...(failed.length > 0 ? { failed } : {}),
          }
          state.lastPushAt = Date.now() // юзер ещё формулирует — окно продлевается
          log(`edit msg${out.msg.message_id}: текст${hasNewMedia ? " + вложения" : ""} подменён в очереди`)
        } else {
          log(`edit msg${out.msg.message_id}: уже не в очереди — игнор`)
        }
      }
    } else {
      // заявка: выгрузить вложения в ФС, пути — в текст (агент приложит к задаче)
      const { attachments, failed } = await downloadAttachment(out.msg)
      const text = composeText(out.text, attachments, failed)
      state.queue.push({
        text,
        chatID: out.msg.chat.id,
        msgID: out.msg.message_id,
        receivedAt: Date.now(),
        ...(attachments.length > 0 ? { attachments } : {}),
        ...(failed.length > 0 ? { failed } : {}),
      })
      state.lastPushAt = Date.now()
      log(`заявка (+${state.queue.length} в очереди${attachments.length > 0 ? `, вложений: ${attachments.length}` : ""}${failed.length > 0 ? `, НЕ выгрузилось: ${failed.length}` : ""}): ${out.text.slice(0, 60)}`)
    }
    save()
  }

  const pollLoop = async (): Promise<void> => {
    while (true) {
      if (stopping) break
      try {
        // long-poll 25с → таймаут fetch должен его покрывать с запасом
        const updates = await tg("getUpdates", { offset: state.offset, timeout: 25, allowed_updates: ["message", "edited_message"] }, 35_000) as TgUpdate[]
        for (const u of updates) {
          if (stopping) break
          await handleUpdate(u)
        }
      } catch (e) {
        if (String(e).includes("Conflict")) log("poll Conflict: конкурирующий getUpdates (второй экземпляр демона поверх systemd?)")
        else log(`poll: ${e}`)
        await sleep(3000)
      }
    }
    loopDone.poll = true
    maybeExit()
  }

  // ——— liveness: луп тайм-аута тишины выходного потока ———

  /** Период проверки тишины; первый тик — сразу после старта. Переопределяется
   *  профилем (silenceTickMs) — инъекция для e2e, прецедент ADR-061
   *  («пейсинг ретраев — инъекция для тестов»). */
  const SILENCE_TICK_MS = 60_000
  const hoursOf = (ms: number): number => Math.round(ms / 3600_000)
  let silenceGate: SilenceGate = { nextAlertAt: 0 }

  /** Громкая гарда вместо фолбэка-свежести: материализация при загрузке делает
   *  ветку недостижимой; при нарушении тишина не должна молча притворяться свежей. */
  const silenceStamp = (): number => {
    const v = state.lastDeliveryAt
    if (v === undefined) throw new Error("метка тишины не материализована при загрузке state")
    return v
  }

  /** Луп тайм-аута тишины (правило опыта «SiYuan-контур», Режим 5 дополнение:
   *  детектор демона-обработчика смотрит на ВЫХОД — доставку/финалы, не на
   *  процесс). ИНВАРИАНТ: никаких kernel/MCP вызовов — только pure-check по
   *  state + ТГ-fetch со своим таймаутом; детектор обязан переживать
   *  деградацию ядра (вечный await клиентов) — он для таких режимов и заведён.
   *  Тик в try/catch (формат runLoop/pollLoop); loopDone.silence выставляется
   *  всегда — краш тика не подвешивает graceful-stop. */
  const silenceLoop = async (): Promise<void> => {
    while (true) {
      if (stopping) break
      try {
        const r = silenceCheck(silenceStamp(), silenceGate, Date.now(), profile.silenceTimeoutMs, profile.silenceAlertRetryMs)
        silenceGate = r.gate
        if (r.alert) {
          const h = hoursOf(r.ageMs)
          log(`тишина выходного потока ~${h} ч (порог ${hoursOf(profile.silenceTimeoutMs)} ч) — доставок и завершённых ходов нет`)
          if (state.chatID !== undefined) {
            const ok = await send(state.chatID, `🚨 ${profile.daemonName}: тишина выходного потока ~${h} ч (порог ${hoursOf(profile.silenceTimeoutMs)} ч) — доставок и завершённых ходов нет при живом процессе. Погляди лог и доску.`)
            if (!ok) log("тишина-алерт не доставлен (ТГ недоступен?) — ретрай по окну")
          } else {
            log("тишина-алерт НЕ доставлен: chatID не залочен — владелец ни разу не писал боту")
          }
        }
      } catch (e) {
        log(`silenceLoop: ${e}`)
      }
      await sleep(profile.silenceTickMs ?? SILENCE_TICK_MS)
    }
    loopDone.silence = true
    maybeExit()
  }

  // ——— автокомпакт вечной сессии ———

  const COMPACT_TICK_MS = 5 * 60_000

  /** Луп автокомпакта (profile.compact; undefined — не стартует): гейты,
   *  вердикт, персист метки и POST — в compactTick (lib/compact), демон даёт
   *  зависимости. Занятость — своя семантика: активный ход, стирт в полёте,
   *  непустая очередь (компакт встал бы за ними — пусть очередь разгрузится
   *  в чистом контексте). Idle — серверная time.idle: run/firing-гейты уже
   *  отсекли фазу стухшего idle (эмпирика hub sessionStatus). */
  const compactLoop = async (): Promise<void> => {
    while (true) {
      if (stopping) break
      const policy = profile.compact
      if (policy === undefined) break
      try {
        await compactTick({
          sessionID: () => state.session,
          busy: () => state.run !== undefined || state.firing !== undefined || state.queue.length > 0,
          probe: async (sessionID) => {
            const p = await compactProbe(restCall, sessionID)
            if (!p.alive) return { idleMs: null, usagePct: null }
            return { idleMs: p.idleAt !== null ? Date.now() - p.idleAt : null, usagePct: p.usagePct }
          },
          lastCompactAt: () => state.lastCompactAt ?? 0,
          persist: (now) => {
            state.lastCompactAt = now
            save()
          },
          fire: (sessionID) => sessionCompact(restCall, sessionID),
          policy,
          log,
        })
      } catch (e) {
        log(`compactLoop: ${e}`)
      }
      await sleep(COMPACT_TICK_MS)
    }
    loopDone.compact = true
    maybeExit()
  }

  const hub: Hub = {
    profile,
    get state() { return state },
    get cfg() { return cfg },
    save,
    log,
    enqueue: (req) => {
      state.queue.push(req)
      state.lastPushAt = Date.now()
      save()
    },
    nextSyntheticMsgID: () => {
      state.nextDraftMsgID = (state.nextDraftMsgID ?? 0) - 1
      return state.nextDraftMsgID
    },
    send,
    deliverWithRetries,
    run: async (): Promise<void> => {
      if (cfg.token === undefined || cfg.token === "" || cfg.owner === undefined || cfg.owner === "") {
        console.error(`hub.json не прочитан или нет token/owner: ${CFG} — заполни по ${profile.here}/README.md`)
        process.exit(1)
      }
      if (!acquireLock({ stateDir: STATE_DIR, onStop: () => { stopping = true } })) {
        console.error(`другой живой инстанс ${profile.daemonName} держит lock — выходим`)
        process.exit(1)
      }
      mkdirSync(dirname(LOG_PATH), { recursive: true })
      gcMedia()
      syncAgentDef()
      if (profile.beforeLoops !== undefined) await profile.beforeLoops(hub)
      // громкий warning инцидент-класса 06.11: чекаут не master без явного
      // RUN_ROOT env — E2E/тестовый прогон трогает прод-state. Пути не
      // переезжают (HOME-механика изолирует state+лог+workspace+media разом
      // и остаётся явной), но молчать о риске нельзя.
      const homeRoot = process.env[profile.homeEnv]?.trim()
      if (homeRoot === undefined || homeRoot === "") {
        const repoRoot = resolve(profile.here, "..")
        // label-логика (trim, «detached») — общая с резолюцией state (lib);
        // null — git-детект не удался (копия чекаута без .git и т.п.): риск
        // прод-контакта тот же, warning тем более — молчать нельзя
        const label = branchLabel(detectBranch(repoRoot))
        const warn = label === null
          ? `${profile.daemonName}: git-детект ветки не удался (${repoRoot}), ${profile.homeEnv} не задан: state/лог/workspace идут в прод; для прогонов задай ${profile.homeEnv}`
          : label === "master"
            ? null
            : `${profile.daemonName}: чекаут «${label}» — не master, ${profile.homeEnv} не задан: state/лог/workspace идут в прод; для прогонов из ветки задай ${profile.homeEnv}`
        if (warn !== null) {
          console.error(warn)
          log(warn)
        }
      }
      const me = await tg("getMe") as { username?: string }
      // терпеливое чтение: чистая машина без зарегистрированного opencode-сервиса
      // не должна валить старт демона (reg перечитывается на каждом restCall —
      // поздняя регистрация подхватится)
      let regUrl: string | undefined
      try { regUrl = (JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string }).url } catch {}
      const m = sessionModel()
      log(`${profile.daemonName}: старт (бот @${me.username ?? "?"}, owner @${cfg.owner}, chat=${state.chatID ?? "—"}, сессия=${state.session ?? "создастся по первому сообщению"}, model=${m.id}${m.variant !== undefined ? `#${m.variant}` : ""}, опенкод=${regUrl ?? "?"})`)
      log(`silence-watch: порог ${hoursOf(profile.silenceTimeoutMs)} ч`)
      void runLoop()
      void pollLoop()
      void silenceLoop()
      if (profile.compact !== undefined) {
        loopDone.compact = false
        log(`compact-watch: idle ${hoursOf(profile.compact.idleMs)} ч, контекст ≥${profile.compact.minContextPct}%`)
        void compactLoop()
      }
      for (const s of profile.sources) {
        void s.start(hub, {
          stopping: () => stopping,
          done: () => {
            loopDone[s.name] = true
            maybeExit()
          },
        })
      }
    },
  }
  return hub
}
