// cron/resolve-body.test.ts — резолв тела крона (задача 20261104072043,
// ревью раунда 1: Common-2 «сжигание слота при сбое ядра»). Инжектируемый
// платформенный tool скриптуется напрямую — без fetch: проверяется, что
// транзиентный сбой ядра (export exists:false при живом блоке, transport
// throw, лаг SQL-индекса доски) даёт исход infra, а не refuse-вердикт.
import { describe, expect, test } from "bun:test"
import { resolveBody, type CronRow, type PlatformTool } from "./cron-lib"

interface ToolCall {
  name: string
  args: Record<string, unknown>
}

/** Скриптованный tool: отвечает по имени тулзы; неожиданный вызов — throw. */
function scriptTool(respond: (call: ToolCall) => unknown): { tool: PlatformTool; calls: ToolCall[] } {
  const calls: ToolCall[] = []
  return {
    calls,
    tool: (<T>(name: string, args: Record<string, unknown>) => {
      calls.push({ name, args })
      const r = respond({ name, args })
      if (r instanceof Error) return Promise.reject(r)
      return Promise.resolve(r as T)
    }) as PlatformTool,
  }
}

const row = (over: Partial<CronRow> = {}): CronRow => ({
  rowId: "20261105210000-row0001", bodyDoc: "20261105210000-docbody", title: "крон X",
  schedule: "5m", status: "вейт", ...over,
})

const BOARD = "20261105210000-board1"
const validMd = "---\ntitle: t\n---\n# t\nпроверь релизы и обнови док"

describe("resolveBody: bound-привязка", () => {
  test("живая привязка, валидное тело → ok", async () => {
    const s = scriptTool((c) => c.name === "export_doc_markdown" ? { exists: true, markdown: validMd } : new Error(`mock: ${c.name}`))
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r).toEqual({ kind: "ok", docId: "20261105210000-docbody", markdown: validMd })
    expect(s.calls.map((c) => c.name)).toEqual(["export_doc_markdown"])
  })

  test("живая привязка, unsupported-тело → refuse с docId", async () => {
    const s = scriptTool((c) => c.name === "export_doc_markdown"
      ? { exists: true, markdown: "---\ntitle: t\n---\n# t\n<!-- unsupported: flavour=affine:database -->" }
      : new Error(`mock: ${c.name}`))
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r.kind).toBe("refuse")
    if (r.kind === "refuse") expect(r.reason).toContain("docId 20261105210000-docbody")
  })

  test("export exists:false при живом блоке → infra (сбоя ядра вердикт не выносится)", async () => {
    const s = scriptTool((c) => {
      if (c.name === "export_doc_markdown") return { exists: false, markdown: "" }
      if (c.name === "doc_exists") return { exists: true }
      return new Error(`mock: ${c.name}`)
    })
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r.kind).toBe("infra")
    if (r.kind === "infra") expect(r.error).toContain("exists:false при живом блоке")
  })

  test("export exists:false и doc_exists кидает transport-ошибку → infra", async () => {
    const s = scriptTool((c) => {
      if (c.name === "export_doc_markdown") return { exists: false, markdown: "" }
      if (c.name === "doc_exists") return new Error("fetch failed")
      return new Error(`mock: ${c.name}`)
    })
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r.kind).toBe("infra")
  })

  test("export кидает → infra (catch-обёртка)", async () => {
    const s = scriptTool(() => new Error("kernel wave"))
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r.kind).toBe("infra")
  })
})

describe("resolveBody: title-fallback (detached / мёртвая привязка)", () => {
  const fallbackScript = (c: ToolCall): unknown => {
    if (c.name === "export_doc_markdown") return { exists: false, markdown: "" }
    if (c.name === "doc_exists") return { exists: false }
    if (c.name === "get_doc") return { box: "20261103224312-fbrf5fp", hpath: "/uwuwu/Кроны uwuwu" }
    if (c.name === "doc_ids_by_hpath") return { ids: ["20261105210000-docbody2"] }
    return new Error(`mock: ${c.name}`)
  }

  test("мёртвая привязка → fallback на единственный док по заголовку → ok", async () => {
    let exportCalls = 0
    const s = scriptTool((c) => {
      if (c.name === "export_doc_markdown") {
        exportCalls++
        return exportCalls === 1 ? { exists: false, markdown: "" } : { exists: true, markdown: validMd }
      }
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row())
    expect(r).toEqual({ kind: "ok", docId: "20261105210000-docbody2", markdown: validMd })
    // hpath собран по контракту CLI: hpath доски + pathSafe(тайтл)
    const hpathCall = s.calls.find((c) => c.name === "doc_ids_by_hpath")
    expect(hpathCall?.args).toEqual({ notebook: "20261103224312-fbrf5fp", path: "/uwuwu/Кроны uwuwu/крон X" })
  })

  test("тайтл со слэшем → U+2215 в hpath (pathSafe-паритет CLI)", async () => {
    let exportCalls = 0
    const s = scriptTool((c) => {
      if (c.name === "export_doc_markdown") {
        exportCalls++
        return exportCalls === 1 ? { exists: false, markdown: "" } : { exists: true, markdown: validMd }
      }
      return fallbackScript(c)
    })
    await resolveBody(s.tool, BOARD, row({ title: "RU/EN ватчер" }))
    expect(s.calls.find((c) => c.name === "doc_ids_by_hpath")?.args.path).toBe("/uwuwu/Кроны uwuwu/RU∕EN ватчер")
  })

  test("0 доков по заголовку → refuse «строка без тела»", async () => {
    const s = scriptTool((c) => {
      if (c.name === "doc_ids_by_hpath") return { ids: [] }
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row({ bodyDoc: "" }))
    expect(r.kind).toBe("refuse")
    if (r.kind === "refuse") expect(r.reason).toContain("строка без тела")
  })

  test("≥2 доков по заголовку → refuse «дубли тел»", async () => {
    const s = scriptTool((c) => {
      if (c.name === "doc_ids_by_hpath") return { ids: ["doc-a", "doc-b"] }
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row({ bodyDoc: "" }))
    expect(r.kind).toBe("refuse")
    if (r.kind === "refuse") expect(r.reason).toContain("doc-a, doc-b")
  })

  test("fallback-док найден, но export дал exists:false → infra (док подтверждён filetree)", async () => {
    const s = scriptTool((c) => {
      if (c.name === "export_doc_markdown") return { exists: false, markdown: "" }
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row({ bodyDoc: "" }))
    expect(r.kind).toBe("infra")
  })

  test("get_doc молча без box/hpath (SQL-лаг индекса) → infra, не refuse", async () => {
    const s = scriptTool((c) => {
      if (c.name === "get_doc") return { box: "", hpath: "" }
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row({ bodyDoc: "" }))
    expect(r.kind).toBe("infra")
    if (r.kind === "infra") expect(r.error).toContain("без box/hpath")
  })

  test("get_doc кидает transport-ошибку → infra", async () => {
    const s = scriptTool((c) => {
      if (c.name === "get_doc") return new Error("kernel wave")
      return fallbackScript(c)
    })
    const r = await resolveBody(s.tool, BOARD, row({ bodyDoc: "" }))
    expect(r.kind).toBe("infra")
  })

  test("нет привязки и заголовка → refuse", async () => {
    const r = await resolveBody(scriptTool(() => new Error("mock")).tool, BOARD, row({ bodyDoc: "", title: "" }))
    expect(r.kind).toBe("refuse")
  })
})
