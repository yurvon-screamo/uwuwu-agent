// tests/platform-wave.test.ts — wave-дисциплина REST post() (срез D задачи
// 20261107132538): «metadata is not ready» — ответ ядра, не транспорт; исчерпание
// ретраев армит per-process окно fail-fast (зеркало Rust WAVE_COOLDOWN_SECS),
// последующие вызовы ядро не трогают. Живое ядро не нужно — fetch скриптуется.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import * as mcp from "../lib/platform"
const { __test } = mcp

const originalFetch = globalThis.fetch

const metadataNotReady = (): Response =>
  new Response(JSON.stringify({ code: 10201, msg: "metadata is not ready" }), { status: 200 })

describe("post() wave-дисциплина", () => {
  beforeEach(() => {
    __test.resetWaveCooldown()
    __test.setWaveRetryBackoffsMs([1, 1])
    __test.setOtherRetryStepMs(1)
  })
  afterEach(() => {
    globalThis.fetch = originalFetch
    __test.setWaveRetryBackoffsMs([2000, 5000])
    __test.setOtherRetryStepMs(700)
    __test.resetWaveCooldown()
  })

  test("metadata-волна: ровно 3 запроса (начальная + 2 ретрая), исчерпание армит окно", async () => {
    let kernelHits = 0
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes("/api/query/sql")) {
        kernelHits++
        return metadataNotReady()
      }
      throw new Error(`mock: неожиданный путь ${input}`)
    }) as typeof fetch

    await mcp.tool("find_doc_by_title", { title: "x" }).catch(() => undefined)
    expect(kernelHits).toBe(3)
  })

  test("окно fail-fast: последующие вызовы падают сразу, ядро не трогают", async () => {
    let kernelHits = 0
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes("/api/query/sql")) {
        kernelHits++
        return metadataNotReady()
      }
      throw new Error(`mock: неожиданный путь ${input}`)
    }) as typeof fetch

    await mcp.tool("find_doc_by_title", { title: "x" }).catch(() => undefined)
    const hitsAfterFirst = kernelHits

    await expect(mcp.tool("find_doc_by_title", { title: "x" })).rejects.toBeInstanceOf(mcp.KernelWaveError)
    expect(kernelHits).toBe(hitsAfterFirst)
  })

  test("wave-ошибка — подкласс KernelUnavailableError (ветвление демонов сохраняется)", async () => {
    let kernelHits = 0
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes("/api/query/sql")) {
        kernelHits++
        return metadataNotReady()
      }
      throw new Error(`mock: неожиданный путь ${input}`)
    }) as typeof fetch

    const err: unknown = await mcp
      .tool("find_doc_by_title", { title: "x" })
      .catch((e: unknown) => e)
    expect(err).toBeInstanceOf(mcp.KernelWaveError)
    expect(err).toBeInstanceOf(mcp.KernelUnavailableError)
    expect((err as Error).name).toBe("KernelWaveError")
    expect(kernelHits).toBe(3)
  })

  test("транспортный отказ окно не армит: вызовы продолжают бить fetch", async () => {
    let kernelHits = 0
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes("/api/query/sql")) {
        kernelHits++
        throw new TypeError("connect ECONNREFUSED")
      }
      throw new Error(`mock: неожиданный путь ${input}`)
    }) as unknown as typeof fetch

    await mcp.tool("find_doc_by_title", { title: "x" }).catch(() => undefined)
    expect(kernelHits).toBeGreaterThan(0)

    const before = kernelHits
    await mcp.tool("find_doc_by_title", { title: "x" }).catch(() => undefined)
    expect(kernelHits).toBeGreaterThan(before)
  })
})
