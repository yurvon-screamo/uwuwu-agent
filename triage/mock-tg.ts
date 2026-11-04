// mock-tg.ts — эмуляция api.telegram.org для досмерджевого протокола демона
// (README «Проверка»): изолированный прогон рядом с живым инстансом, без
// реального Telegram. Хранит очередь апдейтов, отдаёт long-poll getUpdates
// (семантика offset как у TG), пишет sendMessage в журнал. Оркестрация
// сценариев — снаружи, через админ-канал POST /admin/push.
//
//   bun triage/mock-tg.ts --port 8099 [--log /tmp/uwuwu-triage-e2e/sent.jsonl]
//
// Демон подключается env-ом: UWUWU_TRIAGE_TG_API=http://127.0.0.1:<port>.

import { appendFileSync, mkdirSync } from "node:fs"
import { dirname } from "node:path"

const arg = (name: string, def: string): string => {
  const i = process.argv.indexOf(name)
  return i >= 0 && process.argv[i + 1] !== undefined ? process.argv[i + 1]! : def
}
const PORT = Number(arg("--port", "8099"))
const SENT_LOG = arg("--log", "/tmp/uwuwu-triage-e2e/mock-tg-sent.jsonl")
mkdirSync(dirname(SENT_LOG), { recursive: true })

interface MockUpdate {
  update_id: number
  message?: Record<string, unknown>
  edited_message?: Record<string, unknown>
}

let updates: MockUpdate[] = []
let nextUpdateID = 1
let sentCount = 0

const json = (data: unknown): Response => new Response(JSON.stringify(data), { headers: { "content-type": "application/json" } })

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))

Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url)
    const raw = req.method === "POST" ? await req.text() : ""
    if (url.pathname === "/admin/push") {
      const { update } = JSON.parse(raw) as { update?: Record<string, unknown> }
      if (update === undefined) return json({ ok: false, error: "нет update" })
      const id = typeof update.update_id === "number" ? update.update_id : nextUpdateID++
      updates.push({ ...(update as object), update_id: id } as MockUpdate)
      console.log(`[mock-tg] push update_id=${id}`)
      return json({ ok: true, update_id: id })
    }
    if (!url.pathname.startsWith("/bot")) return new Response("not found", { status: 404 })
    const method = url.pathname.slice("/bot".length).split("/").slice(1).join("/")
    const params = raw.trim() === "" ? {} : JSON.parse(raw) as Record<string, unknown>
    if (method === "getUpdates") {
      // семантика TG: отдаём апдейты с update_id >= offset, меньшие считаем подтверждёнными
      const offset = Number(params.offset ?? 0)
      const timeoutSec = Math.min(Number(params.timeout ?? 0), 25)
      const deadline = Date.now() + timeoutSec * 1000
      while (Date.now() < deadline) {
        const ready = updates.filter((u) => u.update_id >= offset)
        if (ready.length > 0) {
          updates = updates.filter((u) => u.update_id < offset)
          return json({ ok: true, result: ready })
        }
        await sleep(300)
      }
      return json({ ok: true, result: [] })
    }
    if (method === "sendMessage") {
      sentCount++
      const line = JSON.stringify({ ts: Date.now(), n: sentCount, chat_id: params.chat_id, text: params.text })
      try { appendFileSync(SENT_LOG, `${line}\n`) } catch {}
      console.log(`[mock-tg] sendMessage #${sentCount} chat=${params.chat_id}: ${String(params.text ?? "").replace(/\n/g, " ⏎ ").slice(0, 100)}`)
      return json({ ok: true, result: { message_id: sentCount } })
    }
    if (method === "getMe") return json({ ok: true, result: { username: "mock_triage_bot", is_bot: true } })
    if (method === "getFile") return json({ ok: true, result: { file_path: "mock/no-file" } })
    return json({ ok: true, result: {} })
  },
})

console.log(`mock-tg: http://127.0.0.1:${PORT} — журнал отправок: ${SENT_LOG}`)
