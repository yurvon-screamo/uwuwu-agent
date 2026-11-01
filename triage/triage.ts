// triage.ts — uwuwu-triage: входная точка задач через Telegram.
// Standalone bun-демон (не плагин — опыт opencode-tg-hub-daemon): long-poll
// getUpdates, заявка владельца → opencode-сессия триажа (glm-5.3-flash,
// изолированный workspace), поллинг финала → ответ сводкой в ТГ.
// Занят → очередь. Запуск: systemd --user (uwuwu-triage.service).

import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join } from "node:path"
import { toTelegramHTML, splitMessage } from "md-to-telegram"
import {
  buildPrompt, classifyUpdate, extractFinal, plainChunks, runTitle, validState,
  type Request, type RunState, type TgUpdate, type TriageState,
} from "./triage-lib"

const HERE = dirname(new URL(import.meta.url).pathname)
const CFG = join(HERE, "hub.json")        // { token, owner, model? } — секрет, gitignored
const PROMPT_MD = join(HERE, "prompt.md") // методичка триажа — в git, правится без кода
const STATE_DIR = join(homedir(), ".local/state/uwuwu-triage")
const STATE_FILE = join(STATE_DIR, "state.json")
const LOCK_DIR = join(STATE_DIR, "lock")
const LOG_PATH = join(homedir(), ".local/share/opencode/log/uwuwu-triage.log")
/** изолированный cwd триаж-сессий: заявки — недоверенный текст, домашний
 *  каталог сессии видеть не должен */
const WORKSPACE = join(homedir(), ".local/share/uwuwu-triage/workspace")
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

const POLL_SEC = Number(process.env.UWUWU_TRIAGE_POLL_SEC ?? 5) // опрос финала прогона
const RUN_TIMEOUT_MS = 15 * 60_000 // прогон дольше — отпускаем и говорим юзеру
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

// конфиг: при прямом запуске — громкий фейл на отсутствии, при импорте в
// тестах — пустышка (тесты герметичны, без прод-зависимостей)
const loadJSON = (p: string, fb: () => unknown): unknown => {
  try { return JSON.parse(readFileSync(p, "utf-8")) }
  catch (e) { if (import.meta.main) throw e; return fb() }
}
const cfg = loadJSON(CFG, () => ({ token: "", owner: "" })) as Partial<Cfg>

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

// ——— одиночный инстанс ———

function acquireLock(): boolean {
  mkdirSync(STATE_DIR, { recursive: true })
  try {
    mkdirSync(LOCK_DIR, { recursive: false })
  } catch {
    try {
      const pid = Number(readFileSync(join(LOCK_DIR, "pid"), "utf-8").trim())
      if (Number.isInteger(pid)) {
        process.kill(pid, 0)
        return false
      }
    } catch { /* мёртвый lock — захватываем */ }
    try {
      rmSync(LOCK_DIR, { recursive: true, force: true })
      mkdirSync(LOCK_DIR, { recursive: false })
    } catch {
      return false
    }
  }
  writeFileSync(join(LOCK_DIR, "pid"), String(process.pid))
  const release = (): void => {
    try { rmSync(LOCK_DIR, { recursive: true, force: true }) } catch {}
    process.exit(0)
  }
  process.on("SIGTERM", release)
  process.on("SIGINT", release)
  return true
}

// ——— Telegram ———

async function tg(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
  const res = await fetch(`https://api.telegram.org/bot${cfg.token}/${method}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(params),
  })
  const json = await res.json() as { ok?: boolean; description?: string; parameters?: { retry_after?: number }; result?: unknown }
  if (json.ok !== true) {
    // parameters (retry_after при 429) пробрасываем наверх для ретрая
    const err = Object.assign(new Error(`TG ${method}: ${json.description ?? "?"}`), { parameters: json.parameters })
    throw err
  }
  return json.result
}

/** markdown агента → TG HTML (AST-конверсия), при провале — plain-чанки.
 *  Уже отправленное не дублируется: html-часть с фолбэком выровнена под
 *  соответствующий кусок исходного текста (как в хабе зоопарка). */
const send = async (chatID: number, text: string): Promise<boolean> => {
  const post = (part: string, html: boolean) =>
    tg("sendMessage", { chat_id: chatID, text: part, ...(html ? { parse_mode: "HTML" } : {}) })
  const attempt = async (part: string, html: boolean): Promise<boolean> => {
    try {
      await post(part, html)
      return true
    } catch (e) {
      const wait = Number((e as { parameters?: { retry_after?: number } }).parameters?.retry_after ?? 0)
      if (wait > 0) {
        try { await sleep((wait + 1) * 1000); await post(part, html); return true } catch {}
      }
      log(`send: ${e}`)
      return false
    }
  }
  let html: string[] = []
  try { html = splitMessage(toTelegramHTML(text).text, { format: "html" }) } catch {}
  if (html.length === 0) {
    for (const c of plainChunks(text)) if (!(await attempt(c, false))) return false
    return true
  }
  const total = html.reduce((s, p) => s + p.length, 0) || 1
  let pos = 0
  const aligned = html.map((p) => {
    const ratio = Math.ceil((p.length / total) * text.length)
    let end = pos + ratio
    if (end > text.length) end = text.length
    // не рвать суррогатную пару
    if (end < text.length) {
      const prev = text.charCodeAt(end - 1)
      if (prev >= 0xd800 && prev <= 0xdbff) end += 1
    }
    const slice = text.slice(pos, end)
    pos = end
    return slice
  })
  for (let i = 0; i < html.length; i++) {
    if (await attempt(html[i]!, true)) continue
    for (let j = i; j < aligned.length; j++) if (!(await attempt(aligned[j]!, false))) return false
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

async function fireSession(req: Request): Promise<RunState> {
  const instructions = readFileSync(PROMPT_MD, "utf-8")
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
  const nonce = Math.random().toString(36).slice(2, 8)
  const ok = await restCall(`/session/${id}/prompt`, {
    method: "POST",
    body: { text: buildPrompt(instructions, req.text, nonce) },
  }).then(() => true).catch(() => false)
  if (!ok) {
    await restCall(`/session/${id}`, { method: "DELETE" }).catch(() => {}) // rollback: сессия-сирота
    throw new Error("prompt не ушёл")
  }
  return { sessionID: id, startedAt: Date.now(), req }
}

/** Дождаться финала (поллинг состояния сессии — как reconcile хаба):
 *  outcome выставлен и idle новее старта → прогон завершён. */
const deliverFinal = async (run: RunState): Promise<void> => {
  let broken = false
  try {
    const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string; time?: { idle?: number } }
    broken = s.outcome !== undefined && s.outcome !== "succeeded"
  } catch { /* статус не достали — попробуем всё равно прочитать сообщения */ }
  let text = ""
  try {
    const msgs = await restCall(`/session/${run.sessionID}/message`)
    if (Array.isArray(msgs)) text = extractFinal(msgs as Parameters<typeof extractFinal>[0], run.startedAt)
  } catch {}
  let body = text !== "" ? text : "(пустой ответ)"
  if (broken) body += "\n\n⚠️ прогон оборван — финал мог не доехать"
  const delivered = await send(run.req.chatID, body)
  log(`финал ${run.sessionID} (${delivered ? "доставлен" : "НЕ доставлен"}): ${body.slice(0, 80)}`)
}

const watchRun = async (run: RunState): Promise<void> => {
  while (true) {
    let done = false
    try {
      const s = await restCall(`/session/${run.sessionID}`) as { outcome?: string; time?: { idle?: number } }
      if (s.outcome !== undefined && typeof s.time?.idle === "number" && s.time.idle >= run.startedAt) done = true
    } catch (e) {
      const status = (e as { status?: number }).status
      if (status === 404 || status === 400) {
        await send(run.req.chatID, "⚠️ сессия триажа пропала — заявка не разобрана, продублируй")
        log(`прогон ${run.sessionID}: сессия пропала`)
        return
      }
    }
    if (done) return await deliverFinal(run)
    if (Date.now() - run.startedAt > RUN_TIMEOUT_MS) {
      await send(run.req.chatID, `⚠️ не дождался финала за ${Math.round(RUN_TIMEOUT_MS / 60_000)} мин — прогон мог зависнуть; задача могла завестись частично, проверь доску`)
      log(`прогон ${run.sessionID}: таймаут — отпустил`)
      return
    }
    await sleep(POLL_SEC * 1000)
  }
}

const runLoop = async (): Promise<void> => {
  while (true) {
    try {
      if (state.run !== undefined) {
        // рестарт демона во время прогона: продолжаем наблюдать ту же сессию
        await watchRun(state.run)
        state.run = undefined
        save()
        continue
      }
      const req = state.queue.shift()
      if (req === undefined) {
        await sleep(2000)
        continue
      }
      save()
      await send(req.chatID, "📥 Принял, разбираю…").catch(() => {})
      try {
        state.run = await fireSession(req)
        save()
        log(`прогон ${state.run.sessionID}: ${req.text.slice(0, 60)}`)
      } catch (e) {
        state.queue.unshift(req) // вернуть в голову очереди, порядок не теряем
        save()
        log(`спавн не удался: ${e} — заявка возвращена в очередь`)
        await send(req.chatID, "⚠️ опенкод недоступен — заявка в очереди, ретрай через 30с").catch(() => {})
        await sleep(30_000)
      }
    } catch (e) {
      log(`runLoop: ${e}`)
      await sleep(5000)
    }
  }
}

// ——— poller ТГ ———

const pollLoop = async (): Promise<void> => {
  while (true) {
    try {
      const updates = await tg("getUpdates", { offset: state.offset, timeout: 25, allowed_updates: ["message"] }) as TgUpdate[]
      for (const u of updates) {
        state.offset = u.update_id + 1
        const out = classifyUpdate(u, cfg.owner ?? "")
        if (out.kind === "skip") {
          save()
          continue
        }
        // привязка чата: первый личный чат владельца; смена — только вручную
        // через сброс state.json (осознанное решение, не авто)
        if (state.chatID === undefined) {
          state.chatID = out.msg.chat.id
          log(`chat locked: ${state.chatID}`)
        } else if (state.chatID !== out.msg.chat.id) {
          log(`чужой чат ${out.msg.chat.id} — skip`)
          save()
          continue
        }
        if (out.kind === "help") {
          await send(state.chatID, HELP).catch(() => {})
        } else if (out.kind === "media") {
          await send(state.chatID, "⚠️ медиа без подписи не разбираю — опиши заявку текстом").catch(() => {})
        } else {
          state.queue.push({ text: out.text, chatID: out.msg.chat.id, msgID: out.msg.message_id, receivedAt: Date.now() })
          log(`заявка (+${state.queue.length} в очереди): ${out.text.slice(0, 80)}`)
        }
        save()
      }
    } catch (e) {
      if (String(e).includes("Conflict")) log("poll Conflict: конкурирующий getUpdates (второй экземпляр демона поверх systemd?)")
      else log(`poll: ${e}`)
      await sleep(3000)
    }
  }
}

// ——— main ———

const main = async (): Promise<void> => {
  if (!acquireLock()) {
    console.error("другой живой инстанс uwuwu-triage держит lock — выходим")
    process.exit(1)
  }
  if (cfg.token === undefined || cfg.token === "" || cfg.owner === undefined || cfg.owner === "") {
    console.error("hub.json: нет token/owner — заполни по README.md")
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
