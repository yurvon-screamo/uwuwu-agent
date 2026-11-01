// triage.ts — uwuwu-triage: входная точка задач через Telegram.
// Standalone bun-демон (не плагин — опыт opencode-tg-hub-daemon): long-poll
// getUpdates, заявка владельца → opencode-сессия триажа (glm-5.3-flash,
// изолированный workspace), поллинг финала → ответ сводкой в ТГ.
// Занят → очередь. Запуск: systemd --user (uwuwu-triage.service).
//
// Гарантии: заявка не теряется при рестарте в любом месте (убирается из
// очереди только после подтверждённого промпта; спавн-окно держит её и в
// очереди, и в state.run). Доставка финала — at-least-once: рестарт посреди
// доставки может задублировать сводку (дедупликацию заявок делает сам
// триаж-агент на доске). Poison-защита: 5 неудачных спавнов → dead-letter
// с полным текстом заявки юзеру.

import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import { toTelegramHTML, splitMessage } from "md-to-telegram"
import { acquireLock } from "../lib/single-instance"
import {
  alignChunks, buildPrompt, classifyUpdate, extractFinal, plainChunks, runTitle, validState,
  type Request, type RunState, type TgUpdate, type TriageState,
} from "./triage-lib"

const HERE = dirname(new URL(import.meta.url).pathname)
const CFG = join(HERE, "hub.json")        // { token, owner, model? } — секрет, gitignored
const PROMPT_MD = join(HERE, "prompt.md") // методичка триажа — в git, правится без кода
const STATE_DIR = join(homedir(), ".local/state/uwuwu-triage")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-triage.log")
/** изолированный cwd триаж-сессий: заявки — недоверенный текст, домашний
 *  каталог сессии видеть не должен */
const WORKSPACE = join(homedir(), ".local/share/uwuwu-triage/workspace")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

// NaN/мусор из env → дефолт; hot-loop поллинга при 0 исключаем
const POLL_SEC = Math.max(1, Number(process.env.UWUWU_TRIAGE_POLL_SEC ?? 5) || 5)
const RUN_TIMEOUT_MS = 15 * 60_000 // прогон дольше — отпускаем и говорим юзеру
const MAX_SPAWN_ATTEMPTS = 5 // dead-letter после N неудачных спавнов подряд
const DELIVER_ATTEMPTS = 3 // попытки доставки финала в ТГ
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const

const HELP = [
  "📥 Триаж uwuwu — входная точка задач.",
  "",
  "Кидай описание задачи одним сообщением: определю проект и тип, проверю на дубли и заведу в канбан uwuwu (или дополню существующую) — отвечу сводкой.",
  "Заявки разбираются по очереди: пока думаю над предыдущей, новые ждут.",
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

// ——— прогоны ———

/** Сессия без побочных эффектов (промпт ещё не отправлялся) — сносим без
 *  страха: агент ничего не начал. */
const dropSession = (sid: string): void => {
  void restCall(`/session/${sid}`, { method: "DELETE" }).catch(() => {})
}

async function createSession(req: Request): Promise<string> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: runTitle(req.text),
      model: cfg.model ?? MODEL,
      location: { directory: WORKSPACE },
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  if (typeof id !== "string" || !id.startsWith("ses_")) throw new Error("сессия не поднялась")
  return id
}

/** Промпт с методичкой; возвращает startedAt (фиксируем ДО отправки — все
 *  assistant-сообщения прогона будут новее). */
async function sendPrompt(sessionID: string, req: Request): Promise<number> {
  const instructions = readFileSync(PROMPT_MD, "utf-8")
  const nonce = Math.random().toString(36).slice(2, 8)
  const startedAt = Date.now()
  await restCall(`/session/${sessionID}/prompt`, {
    method: "POST",
    body: { text: buildPrompt(instructions, req.text, nonce) },
  })
  return startedAt
}

/** Неудача спавна/промпта: transient или poison. attempts считаем в самой
 *  заявке (персист — иначе рестарт в 30с-паузе откатит счётчик и crash-loop
 *  никогда не доедет до dead-letter); MAX_SPAWN_ATTEMPTS — dead-letter:
 *  снимаем и персистим ДО отправки (рестарт не воскресит заявку и не
 *  задублирует сообщение), юзеру уходит полный текст; недоставка — текст
 *  заявки в лог (ничего не теряется молча). */
async function spawnFail(req: Request, e: unknown): Promise<void> {
  const attempts = (req.attempts ?? 0) + 1
  const i = state.queue.findIndex((r) => r.msgID === req.msgID)
  if (i < 0) {
    log(`spawnFail: заявка msg${req.msgID} уже не в очереди — игнор`)
    return
  }
  if (attempts >= MAX_SPAWN_ATTEMPTS) {
    state.queue.splice(i, 1)
    save()
    log(`заявка msg${req.msgID}: ${attempts} неудачных спавнов (${e}) — dead-letter`)
    const ok = await send(req.chatID, `⚠️ Не смог поднять прогон ${attempts} раз — заявка снята с обработки, продублируй позже:\n\n${req.text}`)
    if (!ok) log(`dead-letter msg${req.msgID} НЕ доставлен — текст заявки: ${req.text}`)
    return
  }
  state.queue[i] = { ...req, attempts }
  save()
  log(`спавн не удался (${e}) — ретрай ${attempts}/${MAX_SPAWN_ATTEMPTS - 1} через 30с`)
  await send(req.chatID, `⚠️ Прогон не поднялся (попытка ${attempts} из ${MAX_SPAWN_ATTEMPTS - 1}: ${e instanceof Error ? e.message : String(e)}) — ретрай через 30с`)
  await sleep(30_000)
}

/** Финал прогона → ТГ, с ретраями доставки. ТГ лежит дольше всех попыток —
 *  прогон уже отработал (задача на доске есть), теряем только сводку:
 *  фиксируем полный текст в лог, юзер проверит доску. */
const deliverFinal = async (run: RunState): Promise<void> => {
  let broken = false
  try {
    const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string }
    broken = s.outcome !== undefined && s.outcome !== "succeeded"
  } catch { /* статус не достали — попробуем всё равно прочитать сообщения */ }
  let text = ""
  try {
    const msgs = await restCall(`/session/${run.sessionID}/message`)
    if (Array.isArray(msgs)) text = extractFinal(msgs as Parameters<typeof extractFinal>[0], run.startedAt)
  } catch {}
  let body = text !== "" ? text : "(пустой ответ)"
  if (broken) body += "\n\n⚠️ прогон оборван — финал мог не доехать"
  for (let a = 1; a <= DELIVER_ATTEMPTS; a++) {
    if (await send(run.req.chatID, body)) {
      log(`финал ${run.sessionID} (доставлен, попытка ${a}): ${body.slice(0, 80)}`)
      return
    }
    log(`доставка финала ${run.sessionID}: попытка ${a}/${DELIVER_ATTEMPTS} не прошла`)
    if (a < DELIVER_ATTEMPTS) await sleep(30_000)
  }
  log(`финал ${run.sessionID} НЕ доставлен (ТГ недоступен?) — текст: ${body}`)
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

/** Дождаться финала (поллинг состояния сессии — как reconcile хаба):
 *  outcome выставлен и idle новее старта → прогон завершён. */
const watchRun = async (run: RunState): Promise<"done" | "stopping"> => {
  while (true) {
    if (stopping) return "stopping"
    const st = await sessionStatus(run)
    if (st === "gone") {
      await send(run.req.chatID, "⚠️ сессия триажа пропала — заявка не разобрана, продублируй")
      log(`прогон ${run.sessionID}: сессия пропала`)
      return "done"
    }
    if (st === "done") {
      await deliverFinal(run) // критическая секция: stopping не проверяем — доезжаем
      return "done"
    }
    if (Date.now() - run.startedAt > RUN_TIMEOUT_MS) {
      await send(run.req.chatID, `⚠️ не дождался финала за ${Math.round(RUN_TIMEOUT_MS / 60_000)} мин — прогон мог зависнуть; задача могла завестись частично, проверь доску`)
      log(`прогон ${run.sessionID}: таймаут — отпустил`)
      return "done"
    }
    await sleep(POLL_SEC * 1000)
  }
}

/** Спавн-фаза: заявку из головы очереди поднять в активный прогон.
 *  Заявка НЕ шифтится из очереди до подтверждённого промпта — рестарт в
 *  любой точке окна не теряет её (дубль-прогон нейтрализует дедуп агента).
 *  "idle" — очередь пуста; "started" — прогон активен; "later" — обработан
 *  сбой спавна, ретрай уже запланирован (sleep внутри spawnFail). */
const startRun = async (): Promise<"idle" | "started" | "later"> => {
  const req = state.queue[0]
  if (req === undefined) return "idle"
  // ack — только на первой попытке: при ретраях спавна причины приходят из spawnFail
  if ((req.attempts ?? 0) === 0) await send(req.chatID, "📥 Принял, разбираю…")
  let sessionID: string
  try {
    sessionID = await createSession(req)
  } catch (e) {
    await spawnFail(req, e)
    return "later"
  }
  state.run = { sessionID, startedAt: 0, req }
  save()
  let startedAt: number
  try {
    startedAt = await sendPrompt(sessionID, req)
  } catch (e) {
    dropSession(sessionID)
    state.run = undefined
    save()
    await spawnFail(req, e)
    return "later"
  }
  // промпт подтверждён: заявка принята в работу — убираем из очереди
  const i = state.queue.findIndex((r) => r.msgID === req.msgID)
  if (i >= 0) state.queue.splice(i, 1)
  state.run = { sessionID, startedAt, req }
  const waited = Math.round((Date.now() - req.receivedAt) / 1000)
  log(`прогон ${sessionID}: ${req.text.slice(0, 60)}${waited > 5 ? ` (ждала в очереди ${waited}с)` : ""}`)
  save()
  return "started"
}

const runLoop = async (): Promise<void> => {
  while (true) {
    try {
      if (stopping) break

      // рестарт в спавн-окне: сессия есть, промпт не подтверждён. Сессия без
      // побочных эффектов — сносим; заявка всё ещё в очереди и поднимется заново
      if (state.run !== undefined && state.run.startedAt === 0) {
        dropSession(state.run.sessionID)
        log(`спавн-окно ${state.run.sessionID}: промпт не подтверждён — сессия снесена, заявка перезапустится`)
        state.run = undefined
        save()
      }

      if (state.run !== undefined) {
        if (await watchRun(state.run) === "done") {
          state.run = undefined
          save()
        }
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
  } else if (out.kind === "media") {
    await send(state.chatID, "⚠️ медиа без подписи не разбираю — опиши заявку текстом")
  } else {
    state.queue.push({ text: out.text, chatID: out.msg.chat.id, msgID: out.msg.message_id, receivedAt: Date.now() })
    log(`заявка (+${state.queue.length} в очереди): ${out.text.slice(0, 80)}`)
  }
  save()
}

const pollLoop = async (): Promise<void> => {
  while (true) {
    if (stopping) break
    try {
      // long-poll 25с → таймаут fetch должен его покрывать с запасом
      const updates = await tg("getUpdates", { offset: state.offset, timeout: 25, allowed_updates: ["message"] }, 35_000) as TgUpdate[]
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
  mkdirSync(WORKSPACE, { recursive: true })
  const me = await tg("getMe") as { username?: string }
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string }
  log(`uwuwu-triage: старт (бот @${me.username ?? "?"}, owner @${cfg.owner}, chat=${state.chatID ?? "—"}, model=${(cfg.model ?? MODEL).id}, опенкод=${reg.url ?? "?"})`)
  void runLoop()
  void pollLoop()
}

if (import.meta.main) void main()
