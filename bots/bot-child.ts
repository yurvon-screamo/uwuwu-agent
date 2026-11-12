// bot-child.ts — вход ребёнка раннера uwuwu-bots: один бот = один процесс
// на обкатанном ядре lib/hub.ts (рестарт-гарантии, стирт, dead-letter,
// автокомпакт — без рефакторинга ядра; гашение бота не роняет соседей).
// Супервизор материализует per-бот каталог (hub.json + agent.md + bot-meta.json)
// из док-спеки реестра; ребёнок собирает HubProfile и уходит в hub.run().

import { readFileSync } from "node:fs"
import { join } from "node:path"
import { toTelegramHTML, splitMessage } from "md-to-telegram"
import { createHub, type HubProfile } from "../lib/hub"

interface BotMeta {
  slug: string
  /** имя нативного деф-агента (нормализованный слаг) */
  agentName: string
  sessionTitle: string
  /** каталог сессий и дефа (bot.workspace спеки); undefined — изоляция */
  workspace?: string
  /** политика автокомпакта; отсутствует — дефолт раннера */
  compact?: { idleMs: number; minContextPct: number; cooldownMs: number }
}

const readMeta = (dir: string): BotMeta => JSON.parse(readFileSync(join(dir, "bot-meta.json"), "utf-8")) as BotMeta

const DEFAULT_COMPACT = { idleMs: 4 * 3_600_000, minContextPct: 40, cooldownMs: 3_600_000 }
const SILENCE_TIMEOUT_MS = 24 * 3600_000

const help = (slug: string): string =>
  [
    `🤖 Бот «${slug}» (реестр uwuwu-bots).`,
    "",
    "Пиши как человек — я отвечаю из своей вечной сессии (помню контекст). Занят — после минуты тишины сообщение вбросится в текущий ход, ответ придёт отдельным сообщением.",
    "/reset — стоп-кран: сброс сессии и висящего хода.",
  ].join("\n")

const main = (): void => {
  const slug = process.argv[2]
  if (slug === undefined || slug === "") {
    console.error("usage: bun bot-child.ts <slug>")
    process.exit(1)
  }
  const dir = join(process.env.UWUWU_BOTS_HOME ?? "", ".local/share/uwuwu-bots", slug)
  const meta = readMeta(dir)
  const profile: HubProfile = {
    daemonName: `uwuwu-bots-${slug}`,
    here: dir,
    agentName: meta.agentName,
    sessionTitle: meta.sessionTitle,
    help: help(slug),
    homeEnv: "UWUWU_BOTS_HOME",
    tgApiEnv: "UWUWU_BOTS_TG_API",
    e2eEnv: "UWUWU_BOTS_E2E",
    batchEnv: "UWUWU_BOTS_BATCH_MS",
    pollEnv: "UWUWU_BOTS_POLL_SEC",
    // модель сессии — bot.model спеки в hub.json (cfg.model wins); profile.model —
    // дефолт раннера (формат с variant — паритет dreamer/hub)
    model: { providerID: "zai-coding-plan", id: "glm-5.3-flash" },
    tgFormatter: {
      htmlParts: (text) => {
        try { return splitMessage(toTelegramHTML(text).text, { format: "html" }) } catch { return null }
      },
    },
    sources: [],
    silenceTimeoutMs: SILENCE_TIMEOUT_MS,
    ...(meta.workspace !== undefined ? { workspace: meta.workspace } : {}),
    compact: meta.compact ?? DEFAULT_COMPACT,
  }
  void createHub(profile).run()
}

main()
