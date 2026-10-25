// Живой тест: протухшая сессия восстанавливается одним ретраем.
// Требует UWUWU_AFFINE_* env; помечен skipIf при их отсутствии.
import { describe, expect, test } from "bun:test"
import * as mcp from "./mcp"

const hasEnv = (process.env.UWUWU_AFFINE_TOKEN ?? "") !== ""

describe("mcp session recovery", () => {
  test.skipIf(!hasEnv)("битая сессия → handshake + ретрай проходят", async () => {
    await mcp.connect()
    const before = await mcp.tool<{ matches?: unknown[] }>("find_doc_by_title", { title: "uwuwu Tasks", limit: 1 })
    expect(Array.isArray(before.matches)).toBe(true)
    mcp.__test.expireSession()
    const after = await mcp.tool<{ matches?: unknown[] }>("find_doc_by_title", { title: "uwuwu Tasks", limit: 1 })
    expect(Array.isArray(after.matches)).toBe(true)
  }, 30_000)
})
