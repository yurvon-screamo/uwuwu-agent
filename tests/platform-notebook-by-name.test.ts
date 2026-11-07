// tests/platform-notebook-by-name.test.ts — контракт notebook_id_by_name:
// резолв имени ноутбука в id для getIDsByHPath (инит-блок спавна фабрики).
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

describe("notebook_id_by_name", () => {
  test("живой ноутбук по имени → его id", async () => {
    mockKernel([
      { id: "nbProj", name: "Projects", closed: false },
      { id: "nbExp", name: "Experience", closed: false },
    ])
    expect(await tool<{ id: string }>("notebook_id_by_name", { name: "Experience" })).toEqual({ id: "nbExp" })
  })
  test("закрытый ноутбук не резолвится — пустой id", async () => {
    mockKernel([{ id: "nb1", name: "Projects", closed: true }])
    expect((await tool<{ id: string }>("notebook_id_by_name", { name: "Projects" })).id).toBe("")
  })
  test("неизвестное имя → пустой id (не ошибка)", async () => {
    mockKernel([{ id: "nb1", name: "Projects", closed: false }])
    expect((await tool<{ id: string }>("notebook_id_by_name", { name: "Daily" })).id).toBe("")
  })
})
