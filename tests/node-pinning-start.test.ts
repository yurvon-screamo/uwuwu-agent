// tests/node-pinning-start.test.ts — startPinned: недоступный пиннинг-док
// ретраится с бэкоффом (демон жив, без тиков, без exit-шторма — инцидент
// 13–14.11), violations — исход вызывающего (дефолт = print + exit 1).
import { describe, expect, test } from "bun:test"
import { startPinned, type PinningCheck } from "../lib/node-pinning"

type Tool = Parameters<typeof startPinned>[0]

/** Прагматика типизации скриптовых тулов: тела возвращают конкретные шейпы,
 *  а дженерик-target PinningTool TS без T-референса в теле не верифицирует —
 *  каст ровно на границе (run-time контракты покрывают ассерты тестов). */
const asTool = (fn: (name: string, args: Record<string, unknown>) => unknown): Tool =>
  (async (name, args) => fn(name, args)) as Tool

/** Скриптовый tool: папка uwuwu → один док «Node-pinning» с валидным контентом. */
function okTool(): Tool {
  return asTool((name) => {
    if (name === "list_organize_nodes") {
      return { nodes: [
        { id: "pf:uwuwu", type: "folder", data: "uwuwu", parentId: "nb" },
        { id: "doc:Projects/uwuwu/Node-pinning", type: "doc", data: "doc:Projects/uwuwu/Node-pinning", parentId: "pf:uwuwu" },
      ] }
    }
    if (name === "get_doc") return { title: "Node-pinning" }
    if (name === "export_doc_markdown") {
      return { exists: true, markdown: "## Фабрики\n- kisa: [uwuwu]\n\n## Одиночные демоны\n- cron: kisa\n" }
    }
    return {}
  })
}

const failTool = (failures: number): { tool: Tool; calls: () => number } => {
  let n = 0
  const tool = asTool((name) => {
    n += 1
    if (n <= failures) throw new Error(`пиннинг-док не найден (проба ${n})`)
    return okTool()(name, {})
  })
  return { tool, calls: () => n }
}

const opts = { singletonDaemon: "cron", hostname: "kisa" }
const hooks = (log: string[], sleeps: number[], onViolations?: (c: PinningCheck) => void) => ({
  log: (m: string) => log.push(m),
  sleep: async (ms: number) => { sleeps.push(ms) },
  ...(onViolations !== undefined ? { onViolations } : {}),
})

describe("startPinned: недоступный док — ретрай, не exit", () => {
  test("два сбоя → третья проба успешна: вернулся чек, бэкофф между пробами", async () => {
    const { tool, calls } = failTool(2)
    const log: string[] = []
    const sleeps: number[] = []
    const check = await startPinned(tool, opts, { ...hooks(log, sleeps), retryDelayMs: 60_000 })
    expect(check.nodeName).toBe("kisa")
    expect(check.violations).toEqual([])
    // два ретрая = две паузы; счётчик вызовов tool: неудавшаяся проба гаснет
    // на первом вызове, удачная проходит все три (list/get_doc/export)
    expect(sleeps).toEqual([60_000, 60_000])
    expect(calls()).toBe(5)
    expect(log.length).toBe(2)
    expect(log[0]).toContain("ретрай")
  })

  test("валидный док сразу — без sleep и логов ретрая", async () => {
    const log: string[] = []
    const sleeps: number[] = []
    const check = await startPinned(okTool(), opts, hooks(log, sleeps))
    expect(check.violations).toEqual([])
    expect(sleeps).toEqual([])
    expect(log).toEqual([])
  })
})

describe("startPinned: нарушения пиннинга — исход вызывающего", () => {
  test("чужая нода в доке → violations, onViolations получил чек (дефолт = print + exit 1)", async () => {
    const tool = asTool((name) => {
      if (name === "export_doc_markdown") {
        return { exists: true, markdown: "## Фабрики\n- kisa: [uwuwu]\n\n## Одиночные демоны\n- cron: ДругаяНода\n" }
      }
      return okTool()(name, {})
    })
    const seen: PinningCheck[] = []
    const check = await startPinned(tool, opts, hooks([], [], (c) => { seen.push(c) }))
    expect(seen.length).toBe(1)
    expect(check.violations.join(" ")).toContain("не за «kisa»")
  })
})
