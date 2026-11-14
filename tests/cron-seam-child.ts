// tests/cron-seam-child.ts — спавн-ребёнок интеграции hub-шва крона (инцидент
// 1110; не запускать руками). Env ставит родитель: UWUWU_CRON_DRY_RUN=1 +
// песочница state + UWUWU_HUB_REPO + PATH со stub-gh (ghJson кроны —
// spawnSync("gh") с PATH-резолвом на вызове). tickBoard зовётся напрямую с
// фиксированным now — детерминизм без сна; DRY_RUN гасит все записи и спавны
// (инвариант «DRY_RUN ⇒ ноль реальных действий»).
import { writeFileSync } from "node:fs"
import type { CronState } from "../cron/cron-lib"

const [ready] = process.argv.slice(2)
if (ready === undefined) throw new Error("argv: <ready>")

const cron = await import("../cron/cron")

// фиксированный момент тика: 2026-11-14 04:30 локальной машины —
// разовый слот «04:30» мастера 137 due ровно сейчас, слот «0 5 * * *»
// мастера 440 не совпадает (minute 30 ≠ 0) — только reconcile
const now = new Date(2026, 10, 14, 4, 30)
const state: CronState = {
  crons: {
    // prevFire = вчера 05:00 → next = сегодня 05:00 → клетка «Запланировано»
    // пуста → reconcile-патч planned (гейт «вечного молчуна» prevFire=0 не
    // срабатывает)
    "440": { sessionID: "", lastFiredAt: new Date(2026, 10, 13, 5, 0).getTime(), firedAtIso: "2026-11-13T05:00:00" },
  },
}

await cron.tickBoard(state, { docId: "board:uwuwu", dbId: "db:uwuwu" }, "Задачи uwuwu", now)
writeFileSync(ready, String(process.pid))
