// triage.ts — uwuwu-triage: входная точка задач через Telegram.
// Тонкий вход универсального ядра (lib/hub.ts): профиль демона + вотчер
// черновиков досок (Draft → общая очередь) + draft-хук финала.
// Ядро несёт ТГ-пайп: long-poll, вечная opencode-сессия (нативный деф-агент
// triage), очередь/батчи/стирт/доставка/рестарт-гарантии — см. lib/hub.ts и
// triage/README. Запуск: systemd --user (uwuwu-triage.service).

import { toTelegramHTML, splitMessage } from "md-to-telegram"
import { hostname } from "node:os"
import { mcpConfigured, tool as mcpTool } from "../lib/platform"
import { checkNodePinning, failPinnedStart } from "../lib/node-pinning"
import {
  createHub, type Hub, type HubFinalHook, type HubProfile, type HubSource, type DraftRef, type DraftVerdict,
  type Request,
} from "../lib/hub"
import {
  applyBodyDelta, applyTurns, composeDraftPrompt, draftReady, parseWatchSec, setDraftAck, syncDraftTracker as syncDraftTrackerState, tickOutcome, validTriageExtras,
  DRAFT_MAX_TURNS,
  type DraftRow,
  type DraftTrack,
  type TickFailGate,
} from "./triage-lib"

const HERE = import.meta.dir

// NaN/мусор из env → дефолт; hot-loop вотчера при 0 — kill-switch
const WATCH_SEC = parseWatchSec(process.env.UWUWU_TRIAGE_WATCH_SEC)
/** refresh кэша досок (органи-дерево): часы, не минуты — проход дорогой */
const BOARD_REFRESH_MS = 6 * 3600_000
/** Порог тишины выходного потока: правило владельца (опыт «контур uwuwu»,
 *  Режим 5 дополнение) — волны черновиков ежедневные, сутки максимум. */
const TRIAGE_SILENCE_TIMEOUT_MS = 24 * 3600_000

const HELP = [
  "📥 Триаж uwuwu — входная точка задач.",
  "",
  "Пиши как человеку: поздороваться, спросить, что умею, или кинуть описание задачи — определю проект и тип, проверю на дубли и заведу в канбан uwuwu (или дополню существующую) — отвечу сводкой.",
  "Диалог живёт в одной сессии: помню контекст переписки. Занят — после минуты тишины сообщение вбросится в текущий ход, ответ придёт отдельным сообщением, как только будет готов.",
  "Пиши хоть потоком: склею всё в одну заявку через минуту после последнего сообщения (успей отредактировать — правки подхватятся, пока не начал разбирать).",
  "/reset — стоп-кран: сбросить сессию и висящий ход, начать диалог с чистого контекста.",
].join("\n")

// ——— draft-контур финала (хук ядра) ———

/** Разбор ack логируется на каждом черновичном ходе: канал недоверенный
 *  (LLM-финал), без этой строки эффект seen-ack ненаблюдаем. */
const logAckParse = (hub: Hub, drafts: DraftRef[], acks: Record<string, DraftVerdict>): void => {
  if (drafts.length === 0) return
  const missing = drafts.filter((d) => acks[d.rowId] === undefined).map((d) => d.rowId)
  if (missing.length === drafts.length) {
    hub.log(`draft-ack: не найден (${drafts.length} черновиков) — фолбэк анти-петли`)
    return
  }
  const parsed = drafts
    .filter((d) => acks[d.rowId] !== undefined)
    .map((d) => `${d.rowId}=${acks[d.rowId]}`)
    .join(", ")
  const tail = missing.length > 0 ? `; нет ack: ${missing.join(", ")}` : ""
  hub.log(`draft-ack: распознано ${drafts.length - missing.length}/${drafts.length} (${parsed})${tail}`)
}

/** Ack черновиков батча: свежий snapshot версии тела (MCP-сбой — фолбэк на
 *  трекер, самолечится дельтой следующего тика) + вердикт из финала. */
const recordDraftAcks = async (hub: Hub, drafts: DraftRef[], acks: Record<string, DraftVerdict>): Promise<void> => {
  const tracks = hub.state.drafts as Record<string, DraftTrack> | undefined
  if (tracks === undefined) return
  const now = Date.now()
  let changed = false
  for (const d of drafts) {
    const verdict = acks[d.rowId]
    const t = tracks[d.rowId]
    if (verdict === undefined || t === undefined) continue
    const fresh = await docUpdatedAt(d.docId).catch(() => null)
    if (setDraftAck(t, verdict, fresh ?? t.updatedAt, now)) changed = true
  }
  if (changed) hub.save()
}

/** Хук финала хода: исход «обсуждение» доставляется владельцу (диалог
 *  замыкается той же вечной сессией), draft-трекер учитывается при любом
 *  исходе хода (anti-loop). Порядок — как в моносессионном триаже до выноса
 *  ядра: доставка → лог ack-разбора → трекер → обороты. */
const triageFinalHook: HubFinalHook = async (run, acks, unchat, hub) => {
  const batchDrafts = run.req.drafts ?? []
  if (unchat !== null) {
    const hasDiscussion = batchDrafts.some((d) => acks[d.rowId] === "обсуждение")
    if (hasDiscussion) {
      if (hub.state.chatID !== undefined) {
        await hub.deliverWithRetries(hub.state.chatID, unchat.body, `финал ${run.sessionID} (черновики: обсуждение)`)
      } else {
        hub.log(`обсуждение НЕ доставлено: chatID не залочен — черновик ждёт ответа на доске`)
      }
    }
  }
  logAckParse(hub, batchDrafts, acks)
  await recordDraftAcks(hub, batchDrafts, acks)
  // учёт оборота черновиков — при любом исходе хода (anti-loop:
  // DRAFT_MAX_TURNS холостых → skip-list в вотчере)
  const tracks = hub.state.drafts as Record<string, DraftTrack> | undefined
  if (applyTurns(tracks, batchDrafts.map((d) => d.rowId), Date.now())) hub.save()
}

// ——— вотчер черновиков досок (Draft → общая очередь) ———
// Единый пайп: черновики попадают в ту же state.queue теми же батчами, что
// ТГ-сообщения (могут склеиться в один ход). Буфер 1–2 мин — ДО постановки
// в очередь: тишина по телу задачи (doc.updatedAt) + по sighting'у, кап 10 мин.
// Отказоустойчивость MCP: env нет → вотчер off постоянно; транзиентный сбой
// шлюза → лог и ретрай следующим тиком (self-heal, трекер персистится).

interface BoardRef {
  project: string
  docId: string
  dbId: string
}

/** Кэш досок «Задачи *»: дискавери по организ-дереву дорогой (get_doc на
 *  кандидатов) — часы, не минуты; пере-резолв по сбою чтения доски. */
let boardCache: BoardRef[] = []
let boardCacheAt = 0

async function refreshBoards(hub: Hub): Promise<BoardRef[]> {
  if (boardCache.length > 0 && Date.now() - boardCacheAt < BOARD_REFRESH_MS) return boardCache
  const r = await mcpTool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>(
    "list_organize_nodes", {})
  const nodes = r.nodes ?? []
  const boards: BoardRef[] = []
  for (const f of nodes.filter((n) => n.type === "folder" && typeof n.data === "string" && n.data !== "")) {
    const project = f.data as string
    const docIds = nodes
      .filter((n) => n.type === "doc" && n.parentId === f.id && typeof n.data === "string")
      .map((n) => n.data as string)
    for (const docId of docIds) {
      const d = await mcpTool<{ title?: string }>("get_doc", { docId })
      if (typeof d.title !== "string" || !d.title.startsWith("Задачи ")) continue
      const db = await mcpTool<{ blocks?: { id?: string; flavour?: string }[] }>("read_doc", { docId })
      const dbId = (db.blocks ?? []).find(
        (b) => b.flavour === "platform:database",
      )?.id
      if (typeof dbId === "string" && dbId !== "") boards.push({ project, docId, dbId })
      break // доска проекта одна — первый doc «Задачи …» в папке
    }
  }
  boardCache = boards
  boardCacheAt = Date.now()
  hub.log(`дискавери досок: ${boards.map((b) => b.project).join(", ") || "нет"}`)
  return boards
}

async function readDraftRows(board: BoardRef): Promise<DraftRow[]> {
  const r = await mcpTool<{
    rows?: { rowBlockId?: string; linkedDocId?: string | null; title?: string | null; cells?: Record<string, { value?: string }> }[]
  }>("read_database_cells", { docId: board.docId, databaseBlockId: board.dbId })
  const out: DraftRow[] = []
  for (const row of r.rows ?? []) {
    if (typeof row.rowBlockId !== "string" || row.cells?.Status?.value !== "Draft") continue
    const docId = typeof row.linkedDocId === "string" ? row.linkedDocId : ""
    let title = (row.title ?? "").trim()
    // заголовок линкованной строки живёт в доке-теле (read_database_cells
    // отдаёт пустоту) — доставали для промпта/skip-уведомления
    if (title === "" && docId !== "") {
      const d = await mcpTool<{ title?: string }>("get_doc", { docId }).catch(() => null)
      title = typeof d?.title === "string" ? d.title.trim() : ""
    }
    out.push({ rowId: row.rowBlockId, docId, title })
  }
  return out
}

/** ms последнего обновления тела черновика (буфер тишины); null — не достали. */
async function docUpdatedAt(docId: string): Promise<number | null> {
  if (docId === "") return null
  const d = await mcpTool<{ updatedAt?: string }>("get_doc", { docId })
  if (typeof d.updatedAt !== "string") return null
  const ms = Date.parse(d.updatedAt)
  return Number.isFinite(ms) ? ms : null
}

/** Черновики, уже находящиеся в очереди или активном ходе (не дублируем пуши). */
const draftRowIdsInPipeline = (hub: Hub): Set<string> => {
  const ids = new Set<string>()
  for (const r of hub.state.queue) {
    for (const d of r.drafts ?? []) ids.add(d.rowId)
  }
  if (hub.state.run !== undefined) {
    for (const d of hub.state.run.req.drafts ?? []) ids.add(d.rowId)
  }
  return ids
}

/** Трекер vs доски: новые sighting'и, смена тайтла, чистка ушедших из Draft. */
const syncDraftTracker = (hub: Hub, seen: Map<string, DraftRow>, projects: Map<string, string>): void => {
  const { changed, fresh } = syncDraftTrackerState(hub.state, seen, projects, draftRowIdsInPipeline(hub), Date.now())
  for (const line of fresh) hub.log(line)
  if (changed) hub.save()
}

/** Готовые (тишина/кап) черновики → общая очередь; застрявшие → skip-list. */
const queueReadyDrafts = async (hub: Hub): Promise<void> => {
  const now = Date.now()
  const inFlight = draftRowIdsInPipeline(hub)
  const drafts = hub.state.drafts as Record<string, DraftTrack> | undefined
  for (const [rowId, t] of Object.entries(drafts ?? {})) {
    if (inFlight.has(rowId) || hub.state.draftSkip?.[rowId] !== undefined) continue
    const upd = await docUpdatedAt(t.docId).catch(() => null)
    // дельта тела снимает терминальный ack и даёт свежий цикл (seen-ack)
    if (upd !== null && applyBodyDelta(t, upd)) hub.save()
    if (!draftReady(t, now)) continue
    if (t.turns >= DRAFT_MAX_TURNS) {
      hub.state.draftSkip = hub.state.draftSkip ?? {}
      hub.state.draftSkip[rowId] = now
      hub.save()
      hub.log(`черновик ${rowId}: ${t.turns} холостых оборотов — skip-list`)
      if (hub.state.chatID !== undefined) {
        await hub.send(hub.state.chatID, `⚠️ Черновик ${t.project} ${rowId} «${t.title.slice(0, 60)}» застрял в Draft (${t.turns} разборов) — глянь доску.`)
      } else {
        // чат не залочен (владелец ни разу не писал боту): сигнал не потерять
        hub.log(`skip-уведомление НЕ доставлено: chatID не залочен — черновик ${rowId} в skip-list`)
      }
      continue
    }
    const req: Request = {
      text: composeDraftPrompt(t.project, rowId, t.title),
      chatID: 0,
      msgID: hub.nextSyntheticMsgID(),
      receivedAt: t.seenAt,
      drafts: [{ project: t.project, rowId, docId: t.docId, title: t.title }],
    }
    hub.enqueue(req)
    hub.log(`черновик в очередь: ${t.project} ${rowId} (msg${req.msgID})`)
  }
}

/** Источник промптов «вотчер черновиков»: поллинг досок всех проектов. */
const draftWatcher: HubSource = {
  name: "draft",
  start: async (hub, ctl) => {
    if (!mcpConfigured()) {
      hub.log("вотчер черновиков off: нет UWUWU_HUB_REPO — ТГ-ветка работает как раньше")
      ctl.done()
      return
    }
    if (WATCH_SEC <= 0) {
      hub.log("вотчер черновиков off: UWUWU_TRIAGE_WATCH_SEC=0 (kill-switch)")
      ctl.done()
      return
    }
    let tickGate: TickFailGate = { streak: 0, nextAlertAt: 0 }
    while (true) {
      if (ctl.stopping()) break
      let failed: unknown = null
      try {
        const boards = await refreshBoards(hub)
        const seen = new Map<string, DraftRow>()
        const projects = new Map<string, string>()
        for (const b of boards) {
          try {
            for (const d of await readDraftRows(b)) {
              seen.set(d.rowId, d)
              projects.set(d.rowId, b.project)
            }
          } catch (e) {
            hub.log(`доска ${b.project}: ${e} — пере-резолв кэша`)
            boardCacheAt = 0 // сбой чтения → дискавери на следующем тике
          }
        }
        syncDraftTracker(hub, seen, projects)
        await queueReadyDrafts(hub)
      } catch (e) {
        failed = e
      }
      if (failed === null) {
        tickGate = tickOutcome(tickGate, Date.now(), "ok").gate
      } else {
        // wave-класс (KernelWaveError, «волна деградации ядра — штатная пауза»)
        // появляется с #145; распознавание по имени — сборка не зависит от
        // несмерженного PR, после мерджа ветка активируется сама (переименование
        // класса деградирует к «все сбои считаются» — не хуже прежнего поведения)
        const isWave = failed instanceof Error && failed.name === "KernelWaveError"
        const detail = failed instanceof Error ? `${failed.name}: ${failed.message}` : String(failed)
        const { gate, alert } = tickOutcome(tickGate, Date.now(), isWave ? "wave" : "fail")
        tickGate = gate
        if (isWave) {
          hub.log(`draftLoop: ${detail} — волна ядра, счётчик сбоев не тронут`)
        } else if (alert) {
          hub.log(`draftLoop: ${detail} (${gate.streak} подряд) — ГРОМКИЙ СБОЙ ТИКА`)
          if (hub.state.chatID !== undefined) {
            await hub.send(hub.state.chatID, `⚠️ draftLoop триажа падает ${gate.streak} тиков подряд: ${detail} — черновики не доставляются, глянь лог.`)
          } else {
            hub.log(`тик-алерт НЕ доставлен: chatID не залочен — только лог`)
          }
        } else {
          hub.log(`draftLoop: ${detail}`) // транзиентный сбой шлюза — ретрай следующим тиком
        }
      }
      await new Promise((r) => setTimeout(r, WATCH_SEC * 1000))
    }
    ctl.done()
  },
}

// ——— профиль и вход ———

const profile: HubProfile = {
  daemonName: "uwuwu-triage",
  here: HERE,
  agentName: "triage",
  sessionTitle: "📥 Триаж uwuwu",
  help: HELP,
  homeEnv: "UWUWU_TRIAGE_HOME",
  tgApiEnv: "UWUWU_TRIAGE_TG_API",
  e2eEnv: "UWUWU_TRIAGE_E2E",
  batchEnv: "UWUWU_TRIAGE_BATCH_MS",
  pollEnv: "UWUWU_TRIAGE_POLL_SEC",
  model: { providerID: "zai-coding-plan", id: "glm-5.3-flash" },
  tgFormatter: {
    htmlParts: (text) => {
      try { return splitMessage(toTelegramHTML(text).text, { format: "html" }) } catch { return null }
    },
  },
  sources: [draftWatcher],
  beforeLoops: async (hub) => {
    // пиннинг за нодами (ADR-076): триаж — singleton-класс, второй экземпляр
    // валидировал бы черновики дважды; строка «triage: нода» в доке — контракт
    const pinning = await checkNodePinning(mcpTool, {
      singletonDaemon: "triage",
      envNodeName: process.env.UWUWU_NODE_NAME,
      hostname: hostname(),
    })
    failPinnedStart(pinning)
    hub.log(`пиннинг: нода ${pinning.nodeName} подтверждена`)
  },
  onFinal: triageFinalHook,
  validateState: validTriageExtras,
  silenceTimeoutMs: TRIAGE_SILENCE_TIMEOUT_MS,
  // автокомпакт вечной сессии — пороги из постановки владельца (задача
  // 20261105153731-s6e27f6): 4ч неактивности при контексте ≥40%
  compact: { idleMs: 4 * 3_600_000, minContextPct: 40, cooldownMs: 3_600_000 },
}

const hub = createHub(profile)

const main = async (): Promise<void> => {
  await hub.run()
}

if (import.meta.main) void main()
