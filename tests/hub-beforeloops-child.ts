// tests/hub-beforeloops-child.ts — спавн-ребёнок e2e beforeLoops-хука хаба
// (не запускать руками): мини-профиль на универсальном ядре, режимы по argv.
//   throw — хук бросает: run() отклоняется до getMe/лупов, процесс падает;
//   ok    — хук ждёт gate-файл (родитель держит старт), пишет маркер и
//           отпускает: доказательство «хук между syncAgentDef и лупами»;
//   none  — хук не задан: прежнее поведение, getMe/лупы сразу.
// here — фикстура родителя (hub.json + agent.md), RUN_ROOT и TG-API — env.
import { existsSync, writeFileSync } from "node:fs"
import { createHub, type HubProfile } from "../lib/hub"

const [fixtureDir, mode, gate, marker] = process.argv.slice(2)
if (fixtureDir === undefined || mode === undefined || gate === undefined || marker === undefined) {
  throw new Error("argv: <fixtureDir> <throw|ok|none> <gate> <marker>")
}

const profile: HubProfile = {
  daemonName: "hub-beforeloops-e2e",
  here: fixtureDir,
  agentName: "e2e",
  sessionTitle: "e2e beforeLoops",
  help: "e2e",
  homeEnv: "UWUWU_HUB_BEFORELOOPS_HOME",
  tgApiEnv: "UWUWU_HUB_BEFORELOOPS_TG_API",
  batchEnv: "UWUWU_HUB_BEFORELOOPS_BATCH_MS",
  pollEnv: "UWUWU_HUB_BEFORELOOPS_POLL_SEC",
  model: { providerID: "e2e", id: "e2e" },
  tgFormatter: { htmlParts: () => null },
  sources: [],
  silenceTimeoutMs: 60 * 60 * 1000,
}

if (mode === "throw") {
  profile.beforeLoops = async () => {
    throw new Error("пиннинг-хук e2e: отказ старта до любой поллинг-работы")
  }
} else if (mode === "ok") {
  profile.beforeLoops = async () => {
    const deadline = Date.now() + 30_000
    while (!existsSync(gate)) {
      if (Date.now() > deadline) throw new Error("e2e: родитель не открыл gate за 30с")
      await new Promise((r) => setTimeout(r, 20))
    }
    writeFileSync(marker, String(Date.now()))
  }
}

const hub = createHub(profile)
await hub.run()
