// tests/platform-notebook-ids-by-names.test.ts — контракт notebook_ids_by_names:
// id ноутбуков по именам одним lsNotebooks для getIDsByHPath (инит-блок
// спавна фабрики резолвит пару Experience+Projects за один вызов).
import { afterEach, describe, expect, test } from "bun:test"
import { tool } from "../lib/platform"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

type Notebook = { id?: string; name?: string; closed?: boolean }

function mockKernel(notebooks: Notebook[]): void {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input)
    const respond = (data: unknown): Response =>
      new Response(JSON.stringify({ code: 0, msg: "", data }),
        { headers: { "Content-Type": "application/json" } })
    if (url.includes("/api/notebook/lsNotebooks")) return respond({ notebooks })
    throw new Error(`mock: неожиданный путь ${url}`)
  }) as typeof fetch
}

describe("notebook_ids_by_names", () => {
  test("живые ноутбуки по именам → карта id", async () => {
    mockKernel([
      { id: "nbProj", name: "Projects", closed: false },
      { id: "nbExp", name: "Experience", closed: false },
      { id: "nbDaily", name: "Daily", closed: false },
    ])
    const r = await tool<{ ids: Record<string, string> }>("notebook_ids_by_names", {
      names: ["Experience", "Projects"],
    })
    expect(r.ids).toEqual({ Experience: "nbExp", Projects: "nbProj" })
  })
  test("закрытый ноутбук не резолвится", async () => {
    mockKernel([{ id: "nb1", name: "Projects", closed: true }])
    expect((await tool<{ ids: Record<string, string> }>("notebook_ids_by_names", { names: ["Projects"] })).ids)
      .toEqual({})
  })
  test("неизвестные имена отсутствуют в карте (не ошибка)", async () => {
    mockKernel([{ id: "nb1", name: "Projects", closed: false }])
    expect((await tool<{ ids: Record<string, string> }>("notebook_ids_by_names", { names: ["Daily", "Мусор"] })).ids)
      .toEqual({})
  })
})
