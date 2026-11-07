// tests/cron-dryrun-refuse.test.ts — композиция merge origin/master (sandbox
// DRY_RUN × refuse-ветка): в DRY_RUN refuse-семантика сохраняется (state
// получает lastRefusal — решения не отменяются), но записей нет нигде:
// коммент отказа, state.json и ран-карточка (CLI spawn) гардятся. Сценарий
// гоняется в дочернем процессе (cron-dryrun-child.ts): DRY_RUN — константа
// модуля демона, в общем процессе bun test её не изолировать без утечки в
// соседние файлы (паттерн single-instance-child.ts).
import { describe, expect, test } from "bun:test"
import { existsSync, rmSync } from "node:fs"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "cron-dryrun-child.ts")
const CHILD_STATE_DIR = "/tmp/opencode/cron-dryrun-home/state"

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

describe("tick × DRY_RUN: refuse без записей в доки и без рана", () => {
  test("refuse фиксируется в state, но коммент/state.json/CLI-спавн не случаются", async () => {
    // чистая песочница child: ассерт «state.json не появился» должен быть
    // о именно этом прогоне, а не о хвосте прошлого
    rmSync(CHILD_STATE_DIR, { recursive: true, force: true })
    const report = await runChild()
    expect(report.error ?? "").toBe("")
    // refuse-семантика жива: причина зафиксирована (дедуп повторов),
    // DRY_RUN заменяет записи логом, но решения не отменяет
    const tracked = report.state?.["20261107000000-row0001"]
    expect(tracked?.lastRefusal ?? "").toContain("тело рендерится в пусто")
    // точка композиции: коммент отказа в DRY_RUN не уходит в тело мастера
    expect(report.calls.filter((c) => c.name.includes("appendBlock"))).toEqual([])
    // новый демон вообще не пишет ячейки досок (Last Run/Status сняты)
    expect(report.calls.filter((c) => c.name.includes("item_update"))).toEqual([])
    // ран-карточка не создаётся: ни одного CLI-спавна в DRY_RUN
    expect(report.calls.filter((c) => c.kind === "spawn")).toEqual([])
    // инвариант «DRY_RUN ⇒ ни одной записи» на уровне API (гард saveState,
    // ревью раунд 4 High): state.json не появляется даже в песочнице child —
    // refuse фиксируется только в in-memory state
    expect(existsSync(join(CHILD_STATE_DIR, "state.json"))).toBeFalse()
    // тик дошёл до атрибутов расписания и экспорта тела мастера
    expect(report.calls.some((c) => c.name === "getBlockAttrs")).toBe(true)
    expect(report.calls.some((c) => c.name === "exportMdContent")).toBe(true)
  })
})
