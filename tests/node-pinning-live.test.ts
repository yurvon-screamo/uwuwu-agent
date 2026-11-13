// node-pinning-live.test.ts — живая граница «вики-док ↔ контракт»: реальный
// вики-док (uwuwu-cli read по вики hub) парсится parsePinningDoc.
// Гвардится env-ом ядра (skipIf, паттерн platform-pagination) — в CI ядро
// недоступно и тест скипается. Ассерты — структурные инварианты контура
// (singleton-строки обязательны, фабрики непустые), не точная топология:
// легитимный перепин владельцем тест не ломает.
import { describe, expect, test } from "bun:test"
import { parsePinningDoc } from "../lib/node-pinning"

const hasWiki = (process.env.UWUWU_HUB_REPO ?? "") !== ""

async function readPinningDoc(): Promise<string> {
  const proc = Bun.spawn(["uwuwu-cli", "read", "Projects/uwuwu/Node-pinning"], {
    stdout: "pipe",
    stderr: "pipe",
  })
  const out = await new Response(proc.stdout).text()
  const err = await new Response(proc.stderr).text()
  const code = await proc.exited
  if (code !== 0) throw new Error(`uwuwu-cli read Projects/uwuwu/Node-pinning: code=${code} ${err.slice(0, 200)}`)
  return out
}

describe.skipIf(!hasWiki)("пиннинг-док в вики (живое ядро)", () => {
  test("экспорт ядра парсится контрактом, структурные инварианты контура", async () => {
    const doc = parsePinningDoc(await readPinningDoc())
    // singleton-демона без пина не бывает — строки triage/cron обязательны
    expect(Object.keys(doc.singles)).toEqual(expect.arrayContaining(["triage", "cron"]))
    for (const node of Object.values(doc.singles)) expect(node.trim()).not.toBe("")
    // фабрики непустые: каждая пиненая нода несёт непустой список проектов
    expect(Object.keys(doc.factories).length).toBeGreaterThan(0)
    for (const projects of Object.values(doc.factories)) expect(projects.length).toBeGreaterThan(0)
  })
})
