// tests/cron-dryrun-refuse.test.ts — композиция merge origin/master (sandbox
// DRY_RUN × refuse-ветка): в DRY_RUN refuse-семантика сохраняется (state
// получает lastRefusal — решения не отменяются), но записей в доску нет:
// коммент отказа пропущен (DRY_RUN-контракт «чтение, спавны и записи —
// заменяются логом»; setCells гардится сам, create_comment — точка
// композиции этого репо). Сценарий гоняется в дочернем процессе
// (cron-dryrun-child.ts): DRY_RUN — константа модуля демона, в общем
// процессе bun test её не изолировать без утечки в соседние файлы
// (паттерн single-instance-child.ts).
import { describe, expect, test } from "bun:test"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "cron-dryrun-child.ts")

interface ChildReport {
  error?: string
  state?: Record<string, { lastRefusal?: string }>
  calls: { kind: string; name: string }[]
}

async function runChild(): Promise<ChildReport> {
  const proc = Bun.spawn(["bun", CHILD], {
    env: { ...process.env },
    stdout: "pipe",
    stderr: "pipe",
  })
  const out = await new Response(proc.stdout).text()
  await proc.exited
  const line = out.trim().split("\n").filter((l) => l.startsWith("{")).pop() ?? ""
  return JSON.parse(line) as ChildReport
}

describe("tick × DRY_RUN: refuse без записей в доску", () => {
  test("refuse фиксируется в state, но create_comment/item_update не вызываются", async () => {
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    // refuse-семантика жива: слот сожжён (lastRefusal) — DRY_RUN заменяет
    // записи логом, но решения не отменяет
    const tracked = report.state?.["20261106000000-row0001"]
    expect(tracked?.lastRefusal ?? "").toContain("строка без тела")
    // точка композиции: коммент отказа в DRY_RUN не уходит в док доски
    expect(report.calls.filter((c) => c.name.startsWith("create_comment"))).toEqual([])
    // гард setCells из master: ячейки Last Run/Status не пишутся
    expect(report.calls.filter((c) => c.name.includes("item_update"))).toEqual([])
    // отказ дошёл до title-fallback (get_doc доски + getIDsByHPath)
    expect(report.calls.some((c) => c.name === "getIDsByHPath")).toBe(true)
  })
})
