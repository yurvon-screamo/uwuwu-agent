// triage.ts — uwuwu-triage: входная точка задач через Telegram.
// Standalone bun-демон (не плагин — опыт opencode-tg-hub-daemon): long-poll
// getUpdates, сообщения владельца → ОДНА вечная opencode-сессия диалога
// (нативный деф-агент triage: системный промпт + permissions, материализуется
// демон в workspace/.opencode/agents) — память диалога = контекст сессии
// (компакция рулит), финал хода → ответ в ТГ.
// Занят → очередь. Запуск: systemd --user (uwuwu-triage.service).
//
// Гарантии: сообщение не теряется при рестарте в любом месте (уходит из
// очереди только после подтверждённого промпта; окно отправки держит его и в
// очереди, и в state.run). Доставка финала — at-least-once: рестарт посреди
// доставки может задублировать ответ. Poison-защита: 5 неудачных попыток
// подъёма хода → dead-letter с полным текстом юзеру.

import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import { toTelegramHTML, splitMessage } from "md-to-telegram"
import { acquireLock } from "../lib/single-instance"
import {
  alignChunks, batchReadyAt, classifyUpdate, composeText, dropBatch, extractFinal, joinBatch, plainChunks,
  promptLandedAt, SESSION_TITLE, takeBatch, validState,
  type Request, type RunState, type TgUpdate, type TriageState,
} from "./triage-lib"

const HERE = dirname(new URL(import.meta.url).pathname)
const CFG = join(HERE, "hub.json")        // { token, owner, model? } — секрет, gitignored
const AGENT_DEF_SRC = join(HERE, "agent.md") // нативный деф агента: frontmatter (модель/права) + методичка (системный промпт)
const STATE_DIR = join(homedir(), ".local/state/uwuwu-triage")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-triage.log")
/** изолированный cwd сессии диалога: переписка — недоверенный текст,
 *  домашний каталог сессия видеть не должна; деф агента материализуется
 *  демон в .opencode/agents/ отсюда (нативный механизм конфигурации).
 *  Модель угроз — осознанный трейд-оф диалоговости: текст владельца идёт
 *  в сессию как есть, без маркеров-границ (единственный авторизованный
 *  юзер); сдерживание инструментальное — нативные permissions дефа
 *  (bash: только uwuwu-cli, edit: deny). */
const WORKSPACE = join(homedir(), ".local/share/uwuwu-triage/workspace")
const AGENT_NAME = "triage"
const AGENT_DEF_DST = join(WORKSPACE, ".opencode/agents", `${AGENT_NAME}.md`)
/** выгруженные вложения заявок (скрины/файлы): агент видит их в ФС по путям
 *  из текста заявки и прикладывает к задаче (--attach) */
const MEDIA_DIR = join(homedir(), ".local/share/uwuwu-triage/media")
const MEDIA_TTL_MS = 7 * 24 * 3600_000
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

// NaN/мусор из env → дефолт; hot-loop поллинга при 0 исключаем
const POLL_SEC = Math.max(1, Number(process.env.UWUWU_TRIAGE_POLL_SEC ?? 5) || 5)
const RUN_TIMEOUT_MS = 15 * 60_000 // ход дольше — отпускаем и говорим юзеру
const MAX_SPAWN_ATTEMPTS = 5 // dead-letter после N неудачных попыток хода подряд
const DELIVER_ATTEMPTS = 3 // попытки доставки финала в ТГ
const BROKEN_SESSION_RESET = 3 // оборванных финалов подряд → пересоздание сессии
const RESET_WINDOW_MS = 30 * 60_000 // пересоздания чаще окна = стабильная деградация
const RESET_PAUSE_MS = 30 * 60_000 // пауза обработки при серии пересозданий
/** дебаунс батча: поток сообщений склеивается в один ход, юзер получает окно
 *  на редактирование (edited_message подменяет текст, пока батч в очереди) */
const BATCH_DELAY_MS = Math.max(0, Number(process.env.UWUWU_TRIAGE_BATCH_MS ?? 60_000) || 60_000)
const BATCH_MAX_MS = 5 * 60_000 // самое старое сообщение не ждёт дольше
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const

const HELP = [
  "📥 Триаж uwuwu — входная точка задач.",
  "",
  "Пиши как человеку: поздороваться, спросить, что умею, или кинуть описание задачи — определю проект и тип, проверю на дубли и заведу в канбан uwuwu (или дополню существующую) — отвечу сводкой.",
  "Диалог живёт в одной сессии: помню контекст переписки. Занят — сообщение встанет в очередь.",
  "Пиши хоть потоком: склею всё в одну заявку через минуту после последнего сообщения (успей отредактировать — правки подхватятся, пока не начал разбирать).",
  "/reset — начать диалог заново с чистого контекста (методичка всегда актуальная).",
].join("\n")

interface Cfg { token: string; owner: string; model?: { providerID: string; id: string } }

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

let stopping = false
// координированный останов: оба цикла доходят до безопасной точки, процесс
// выходит сам; grace-таймаут в acquireLock — страховка от зависшего drainage
const loopDone = { run: false, poll: false }
const maybeExit = (): void => {
  if (loopDone.run && loopDone.poll) {
    save()
    process.exit(0)
  }
}

// конфиг: ошибки чтения не роняем на импорте — main() скажет человечно
let cfg: Partial<Cfg> = {}
try { cfg = JSON.parse(readFileSync(CFG, "utf-8")) as Partial<Cfg> } catch {}

// state: терпеливое чтение (битый файл не должен крашить always-on демон)
// + атомарная запись (tmp+rename) с бэкапом
let state: TriageState = { offset: 0, queue: [] }
if (existsSync(STATE_FILE)) {
  try { state = validState(JSON.parse(readFileSync(STATE_FILE, "utf-8"))) }
  catch (e) { log(`state.json битый — стартую с пустого (${e})`) }
}
const save = (): void => {
  mkdirSync(STATE_DIR, { recursive: true })
  if (existsSync(STATE_FILE)) {
    try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
  }
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 1))
  renameSync(tmp, STATE_FILE)
}

// ——— Telegram ———

async function tg(method: string, params: Record<string, unknown> = {}, timeoutMs = 30_000): Promise<unknown> {
  const res = await fetch(`https://api.telegram.org/bot${cfg.token}/${method}`, {
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

/** markdown агента → TG HTML (AST-конверсия); упавшая html-часть и всё после
 *  неё доезжает исходным текстом (куски выровнены) — уже отправленное не
 *  дублируется, теги в чат не утекают. Ошибки глотает: false = не доставлено
 *  (никогда не реджектит — вызывающие ветки сами решают, что делать). */
const send = async (chatID: number, text: string): Promise<boolean> => {
  let html: string[] = []
  try { html = splitMessage(toTelegramHTML(text).text, { format: "html" }) } catch {}
  if (html.length === 0) return sendPlain(chatID, text)
  const aligned = alignChunks(html, text)
  for (let i = 0; i < html.length; i++) {
    if (await attemptSend(chatID, html[i]!, true)) continue
    for (let j = i; j < aligned.length; j++) {
      if (!(await attemptSend(chatID, aligned[j]!, false))) return false
    }
    return true
  }
  return true
}

// ——— opencode REST (registration-файл фонового сервиса; перечитываем на
//  каждый вызов — файл меняется при рестарте сервиса, кэш = stale url) ———

async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = "Basic " + Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")
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

async function sessionMessages(sessionID: string): Promise<Parameters<typeof extractFinal>[0]> {
  const msgs = await restCall(`/session/${sessionID}/message`)
  return Array.isArray(msgs) ? msgs as Parameters<typeof extractFinal>[0] : []
}

// ——— вечная сессия диалога ———

/** Материализовать нативный деф агента в воркспейс (если изменился) —
 *  сессии, созданные в этом location с agent:"triage", получают его
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
    log(`деф агента ${AGENT_NAME} материализован (${src.length} байт)`)
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

/** Единая сессия диалога: создаётся один раз, переживает рестарты демона
 *  (контекст всей переписки + компакция); пропала (снесли в TUI) — пересоздаём,
 *  контекст теряется: лог + честное уведомление юзеру.
 *  Деф горячий (эмпирика 2.2.5, чистые сессии без отравленной истории):
 *  сервер перечитывает agent-def на каждом промпте — правка agent.md в репо
 *  доезжает живой сессии следующим же сообщением (демон материализует деф в
 *  ensureSession). Гочи проб: не гонять видимость правил в сессии, где модель
 *  уже отрицала их — история диалога сильнее системного обновления.
 *  Модель живой сессии нативно следует hub.json (switchModel). */
const ensureSession = async (): Promise<string> => {
  if (state.session !== undefined) {
    const alive = await sessionAlive(state.session)
    if (alive === true) {
      syncAgentDef() // правка дефа в репо → живая сессия следующим сообщением
      try { await restCall(`/session/${state.session}/model`, { method: "POST", body: { model: cfg.model ?? MODEL } }) } catch (e) { log(`switchModel: ${e}`) }
      return state.session
    }
    if (alive === null) return state.session // неизвестно — не пересоздаём
    log(`сессия ${state.session} пропала — пересоздаю (контекст переписки потерян)`)
    await recreateSession(true)
    return state.session!
  }
  await recreateSession(false)
  return state.session!
}

const recreateSession = async (notify: boolean): Promise<void> => {
  syncAgentDef()
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: SESSION_TITLE,
      agent: AGENT_NAME,
      // NB: model из дефа не применяется к primary-сессиям — передаём явно
      model: cfg.model ?? MODEL,
      location: { directory: WORKSPACE },
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  if (typeof id !== "string" || !id.startsWith("ses_")) throw new Error("сессия не поднялась")
  state.session = id
  state.broken = 0
  save()
  log(`сессия диалога: ${id}`)
  if (notify && state.chatID !== undefined) {
    await send(state.chatID, "ℹ️ Сессия диалога пересоздана — контекст переписки сброшен.")
  }
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
    const ok = await send(req.chatID, `⚠️ Не смог поднять ход ${attempts} раз — заявка снята с обработки, продублируй позже:\n\n${req.text}`)
    if (!ok) log(`dead-letter msg${req.msgID} НЕ доставлен (текст заявки выше в этом логе)`)
    return
  }
  rep.attempts = attempts // счётчик на представителе: состав батча в очереди не трогаем
  state.firing = undefined // полёт не состоялся, батч в очереди — правки снова можно
  save()
  log(`ход не удался (${e}) — ретрай ${attempts}/${MAX_SPAWN_ATTEMPTS - 1} через 30с`)
  await send(req.chatID, `⚠️ Прогон не поднялся (попытка ${attempts} из ${MAX_SPAWN_ATTEMPTS - 1}: ${e instanceof Error ? e.message : String(e)}) — ретрай через 30с`)
  await sleep(30_000)
}

/** Финал хода → ТГ, с ретраями доставки. ТГ лежит дольше всех попыток —
 *  ход уже отработал (задача на доске есть), теряем только ответ:
 *  фиксируем полный текст в лог, юзер проверит доску.
 *  Эскалация деградации: 3 оборванных финала подряд (outcome ≠ succeeded —
 *  контекст перерос компакцию, модель приказала жить) → сессию пересоздаём
 *  (модель возьмётся свежая из конфига), текущее сообщение — обратно в очередь. */
const deliverFinal = async (run: RunState): Promise<void> => {
  let broken = false
  try {
    const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string }
    broken = s.outcome !== undefined && s.outcome !== "succeeded"
  } catch { /* статус не достали — попробуем всё равно прочитать сообщения */ }
  let text = ""
  try { text = extractFinal(await sessionMessages(run.sessionID), run.startedAt) } catch {}
  let body = text !== "" ? text : "(пустой ответ)"
  if (broken) body += "\n\n⚠️ прогон оборван — финал мог не доехать"
  for (let a = 1; a <= DELIVER_ATTEMPTS; a++) {
    if (await send(run.req.chatID, body)) {
      log(`финал ${run.sessionID} (доставлен, попытка ${a}): ${body.slice(0, 80)}`)
      break
    }
    log(`доставка финала ${run.sessionID}: попытка ${a}/${DELIVER_ATTEMPTS} не прошла`)
    if (a < DELIVER_ATTEMPTS) await sleep(30_000)
    else log(`финал ${run.sessionID} НЕ доставлен (ТГ недоступен?) — текст: ${body}`)
  }
  if (!broken) {
    state.broken = 0
    save()
    return
  }
  state.broken = (state.broken ?? 0) + 1
  if (state.broken < BROKEN_SESSION_RESET) {
    save()
    return
  }
  log(`сессия ${run.sessionID}: ${state.broken} оборванных финалов подряд — пересоздаю`)
  // СБРОС: все мутации state — ДО любых await (уведомление может упасть/крах):
  // run снесён сразу, иначе рестарт в окне сочтёт ход подтверждённым по тексту
  // в старой сессии и съест сообщение из очереди (ломая «обработаю заново»)
  state.broken = 0
  state.session = undefined
  state.run = undefined
  // сообщение не отработано нормально — вернуть в голову очереди начисто
  state.queue.unshift({ text: run.req.text, chatID: run.req.chatID, msgID: run.req.msgID, receivedAt: run.req.receivedAt })
  // circuit breaker: пересоздания чаще раза в 30 мин = стабильная деградация
  // (модель выпилена/контекст не лезет) — пауза обработки, спам-цикл разорван
  const now = Date.now()
  const rapid = state.lastResetAt !== undefined && now - state.lastResetAt < RESET_WINDOW_MS
  state.lastResetAt = now
  if (rapid) {
    state.pausedUntil = now + RESET_PAUSE_MS
    log(`пауза обработки до ${new Date(state.pausedUntil).toISOString()} — серия пересозданий`)
  }
  save()
  if (state.chatID !== undefined) {
    const msg = state.pausedUntil !== undefined && state.pausedUntil > now
      ? "⚠️ Диалог стабильно деградирует — пересоздал сессию и приостановил обработку на 30 мин (сообщение в очереди). Глянь лог uwuwu-triage."
      : "⚠️ Диалог деградировал (3 сбоя подряд) — пересоздаю сессию с чистого листа, сообщение обработаю заново."
    await send(state.chatID, msg)
  }
}

/** Одна проверка сессии: ещё исполняется / завершена / пропала. */
const sessionStatus = async (run: RunState): Promise<"running" | "done" | "gone"> => {
  try {
    const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string; time?: { idle?: number } }
    if (s.outcome !== undefined && typeof s.time?.idle === "number" && s.time.idle >= run.startedAt) return "done"
    return "running"
  } catch (e) {
    const status = (e as { status?: number }).status
    return status === 404 || status === 400 ? "gone" : "running" // неизвестно — не блокируем
  }
}

/** Дождаться финала хода (поллинг состояния сессии — как reconcile хаба):
 *  outcome выставлен и idle новее старта хода → ход завершён. */
const watchRun = async (run: RunState): Promise<"done" | "stopping"> => {
  while (true) {
    if (stopping) return "stopping"
    const st = await sessionStatus(run)
    if (st === "gone") {
      await send(run.req.chatID, "⚠️ сессия триажа пропала — сообщение не разобрано, продублируй")
      log(`ход ${run.sessionID}: сессия пропала`)
      return "done"
    }
    if (st === "done") {
      await deliverFinal(run) // критическая секция: stopping не проверяем — доезжаем
      return "done"
    }
    if (Date.now() - run.startedAt > RUN_TIMEOUT_MS) {
      await send(run.req.chatID, `⚠️ Не дождался ответа за ${Math.round(RUN_TIMEOUT_MS / 60_000)} мин — ход мог зависнуть; задача могла завестись частично, проверь доску`)
      log(`ход ${run.sessionID}: таймаут — отпустил`)
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
  const req = joinBatch(items)
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
  log(`ход ${sessionID} (${items.length} сообщ.): ${req.text.slice(0, 60)}${waited > 5 ? ` (ждало в очереди ${waited}с)` : ""}`)
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

      // circuit breaker: серия пересозданий сессии → пауза (сообщения копятся
      // в очереди, после истечения пойдут в свежую сессию)
      if (state.pausedUntil !== undefined) {
        if (Date.now() < state.pausedUntil) {
          await sleep(Math.min(state.pausedUntil - Date.now(), 30_000))
          continue
        }
        state.pausedUntil = undefined
        save()
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

/** Выгрузить фото/документ из сообщения в MEDIA_DIR. Возвращает вложения и
 *  маркеры неудач (текст заявки честно скажет агенту, что не выгрузилось). */
const downloadAttachment = async (m: NonNullable<TgUpdate["message"]>): Promise<{ attachments: { path: string; name: string }[]; failed: string[] }> => {
  const out: { attachments: { path: string; name: string }[]; failed: string[] } = { attachments: [], failed: [] }
  const save = async (fileId: string | undefined, name: string): Promise<void> => {
    if (fileId === undefined) { out.failed.push(name); return }
    try {
      const f = await tg("getFile", { file_id: fileId }) as { file_path?: string }
      if (typeof f.file_path !== "string") throw new Error("нет file_path")
      const res = await fetch(`https://api.telegram.org/file/bot${cfg.token}/${f.file_path}`, {
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
  // фото: последний элемент массива = максимальное разрешение
  const photo = Array.isArray(m.photo) ? m.photo[m.photo.length - 1] : undefined
  if (photo !== undefined) await save(photo.file_id, `${m.message_id}.jpg`)
  if (m.document?.file_id !== undefined) {
    // имя файла из ТГ санитайзим (слэши/управляющие), режем до 80
    const safe = (m.document.file_name ?? "file").replace(/[/\\\x00-\x1f]/g, "_").slice(0, 80)
    await save(m.document.file_id, `${m.message_id}_${safe}`)
  }
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
    await send(state.chatID, HELP)
  } else if (out.kind === "reset") {
    if (state.run !== undefined) {
      await send(state.chatID, "⏳ Сейчас разбираю сообщение — пришли /reset после моего ответа.")
    } else {
      state.session = undefined
      state.pausedUntil = undefined
      state.broken = 0
      save()
      log("сессия диалога сброшена командой /reset")
      await send(state.chatID, "♻️ Сессия сброшена — следующее сообщение начнёт диалог с чистого контекста.")
    }
  } else if (out.kind === "media") {
    await send(state.chatID, "⚠️ Такой тип медиа не разбираю (голос/видео/стикер) — опиши словами или пришли фото/файл")
  } else if (out.kind === "edit") {
    // правка в окне дебаунса: подменяем текст, пока батч не ушёл агенту
    if (state.firing !== undefined && state.firing.msgIDs.includes(out.msg.message_id)) {
      log(`edit msg${out.msg.message_id}: батч уже в полёте — правка не принята`)
      await send(state.chatID, "⏳ Успел раньше меня — сообщение уже у агента. Пришли правку новым сообщением.")
    } else {
      const i = state.queue.findIndex((r) => r.msgID === out.msg.message_id)
      if (i >= 0) {
        // правка может нести НОВОЕ вложение (заменил фото) — выгружаем; текст
        // пересобираем с маркерами (пути в ФС — источник для --attach агента)
        const hasNewMedia = (Array.isArray(out.msg.photo) && out.msg.photo.length > 0) || out.msg.document?.file_id !== undefined
        const dl = hasNewMedia ? await downloadAttachment(out.msg) : { attachments: [], failed: [] }
        const cur = state.queue[i]!
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

// ——— main ———

const main = async (): Promise<void> => {
  if (cfg.token === undefined || cfg.token === "" || cfg.owner === undefined || cfg.owner === "") {
    console.error(`hub.json не прочитан или нет token/owner: ${CFG} — заполни по ${HERE}/README.md`)
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: () => { stopping = true } })) {
    console.error("другой живой инстанс uwuwu-triage держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  gcMedia()
  syncAgentDef()
  const me = await tg("getMe") as { username?: string }
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string }
  log(`uwuwu-triage: старт (бот @${me.username ?? "?"}, owner @${cfg.owner}, chat=${state.chatID ?? "—"}, сессия=${state.session ?? "создастся по первому сообщению"}, model=${(cfg.model ?? MODEL).id}, опенкод=${reg.url ?? "?"})`)
  void runLoop()
  void pollLoop()
}

if (import.meta.main) void main()
