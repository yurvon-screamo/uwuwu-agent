// hub-silence-child.ts — спавн-ребёнок e2e алерт-пути тишины (не запускать
// руками): мини-профиль на универсальном ядре с тестовым порогом. here —
// фикстура родителя (hub.json + agent.md), RUN_ROOT — env UWUWU_HUB_SILENCE_HOME
// (песочница state/лог/media), TG — env UWUWU_HUB_SILENCE_TG_API (mock-TG).
import { writeFileSync } from "node:fs"
import { createHub, type HubProfile } from "../lib/hub"

const [fixtureDir, ready, timeoutMs] = process.argv.slice(2)
if (fixtureDir === undefined || ready === undefined || timeoutMs === undefined) {
  throw new Error("argv: <fixtureDir> <ready> <timeoutMs>")
}

const profile: HubProfile = {
  daemonName: "hub-silence-e2e",
  here: fixtureDir,
  agentName: "e2e",
  sessionTitle: "e2e silence",
  help: "e2e",
  homeEnv: "UWUWU_HUB_SILENCE_HOME",
  tgApiEnv: "UWUWU_HUB_SILENCE_TG_API",
  batchEnv: "UWUWU_HUB_SILENCE_BATCH_MS",
  pollEnv: "UWUWU_HUB_SILENCE_POLL_SEC",
  model: { providerID: "e2e", id: "e2e" },
  tgFormatter: { htmlParts: () => null },
  sources: [],
  silenceTimeoutMs: Number(timeoutMs),
  silenceTickMs: 500, // инъекция: реальный второй тик в окне теста
  silenceAlertRetryMs: 500, // инъекция: изолированный mark-wiring (см. e2e-тест)
}

const hub = createHub(profile)
await hub.run()
writeFileSync(ready, String(process.pid))
