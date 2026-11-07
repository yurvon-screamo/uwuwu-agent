// lead.ts — uwuwu-lead: агент-демон «Lead» — ночной аудит досок и сессий
// платформы uwuwu (зависшие агенты, техдолг, отклонения от целей) + ручной
// диалог владельца в ТГ.
// Тонкий вход универсального ядра (lib/hub.ts): профиль демона + ночной
// шедулер как источник промптов в ТУ ЖЕ вечную сессию (мастер-сессия —
// постоянный контекст аудитов, механика триажа). Запуск: systemd --user
// (uwuwu-lead.service); крон-контур uwuwu-cron не используется — он спавнит
// отдельную сессию на ран-карточку, что убило бы постоянный контекст.

import { toTelegramHTML, splitMessage } from "md-to-telegram"
import {
  createHub, type HubProfile, type HubSource, type Request,
} from "../lib/hub"
import { auditDue, composeAuditPrompt, localDateKey, parseAuditAt, validLeadExtras, type AuditAt } from "./lead-lib"

const HERE = import.meta.dir

/** Тик шедулера: гейт файра дешёвый (локальное время + state-ключ). */
const AUDIT_TICK_MS = 60_000

const HELP = [
  "🌙 Lead uwuwu — ночной аудитор платформы.",
  "",
  "Раз в сутки ночью (04:30 по локалке) провожу аудит всех проектов: зависшие агенты и статусы, техдолг (завожу задачи Draft'ами), отклонения от целей, повторяющиеся инциденты — и шлю сводку сюда. Тихие сутки — молчу.",
  "Пиши как человеку: спросить статус проекта, пожаловаться на зависшую задачу, попросить разобраться — отвечу по данным платформы, контекст диалога помню.",
  "/reset — стоп-кран: сбросить сессию и висящий ход, начать диалог с чистого контекста.",
].join("\n")

// ——— источник промптов «ночной шедулер» ———

/** Источник «ночной аудит»: локальное время ≥ auditAt, сутки не отработаны
 *  (state.cron.lastFireDate), очередь пуста (не смешивать аудит с батчем
 *  юзера) → промпт в общую очередь вечной сессии. chatID = залоченный чат
 *  владельца (до первичного /start — 0: финал уйдёт в лог, сводка не
 *  потеряется доской — аудиты идемпотентны). Пропущенный слот доезжает
 *  догоном при старте демона. */
const auditScheduler: HubSource = {
  name: "audit",
  start: async (hub, ctl) => {
    const raw = process.env.UWUWU_LEAD_AUDIT_AT
    const at: AuditAt = parseAuditAt(raw) ?? (() => {
      hub.log(`UWUWU_LEAD_AUDIT_AT «${raw}» кривое — дефолт 04:30`)
      return { hour: 4, minute: 30 }
    })()
    while (true) {
      if (ctl.stopping()) break
      try {
        const now = new Date()
        if (auditDue(now, at, hub.state.cron?.lastFireDate) && hub.state.queue.length === 0) {
          const today = localDateKey(now)
          // mark-then-enqueue (at-most-once): краш между меткой и enqueue —
          // пропуск суток, не дубль-аудит; повторный ход агента дешёвого
          // вреда не несёт, а повторная сводка — шум
          hub.state.cron = { lastFireDate: today }
          hub.save()
          const req: Request = {
            text: composeAuditPrompt(today),
            chatID: hub.state.chatID ?? 0,
            msgID: hub.nextSyntheticMsgID(),
            receivedAt: Date.now(),
            allowSilent: true, // тихие сутки — молчание, не «(пустой ответ)»
          }
          hub.enqueue(req)
          hub.log(`ночной аудит в очередь (сутки ${today}, chat=${req.chatID}, msg${req.msgID})`)
        }
      } catch (e) {
        hub.log(`auditLoop: ${e}`) // транзиентный сбой — ретрай следующим тиком
      }
      await new Promise((r) => setTimeout(r, AUDIT_TICK_MS))
    }
    ctl.done()
  },
}

// ——— профиль и вход ———

const profile: HubProfile = {
  daemonName: "uwuwu-lead",
  here: HERE,
  agentName: "lead",
  agentDefFile: "lead-agent.md",
  sessionTitle: "🌙 Lead uwuwu",
  help: HELP,
  homeEnv: "UWUWU_LEAD_HOME",
  tgApiEnv: "UWUWU_LEAD_TG_API",
  e2eEnv: "UWUWU_LEAD_E2E",
  batchEnv: "UWUWU_LEAD_BATCH_MS",
  pollEnv: "UWUWU_LEAD_POLL_SEC",
  // ночной аудит — аналитика пачкой, ночь не торопится: паритет с дримером
  // (glm-5.3#max); hub.json model переопределяет целиком
  model: { providerID: "zai-coding-plan", id: "glm-5.3", variant: "max" },
  tgFormatter: {
    htmlParts: (text) => {
      try { return splitMessage(toTelegramHTML(text).text, { format: "html" }) } catch { return null }
    },
  },
  sources: [auditScheduler],
  validateState: validLeadExtras,
}

const hub = createHub(profile)

const main = async (): Promise<void> => {
  await hub.run()
}

if (import.meta.main) void main()
