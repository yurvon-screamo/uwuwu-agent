// tests/platform-mcp-errors.test.ts — контракт ошибок mcpRpc (баг «JSON Parse
// error маскирует причину», задача 20261105140450): не-JSON/конверт ядра дают
// честную ошибку со статусом и сниппетом; 404 при отправленной сессии лечится
// одним re-handshake, без цикла. Живое ядро не нужно — fetch скриптуется.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import * as mcp from "../lib/platform"
const { __test } = mcp

const originalFetch = globalThis.fetch

interface McpCall {
  method: string
  hasSession: boolean
  sessionId: string
  /** page из arguments tools/call (database render); 0 — не render-вызов */
  page: number
}

const AV_ID = "20260101000000-abcdef"

/** Скриптованный fetch: REST-лега ядра (/api/query/sql) отвечает кодом 0,
 *  лега /mcp — по скрипту; mcp-вызовы фиксируются (method/sessionId).
 *  Сигнатура вызова (AbortSignal) доступна через recorded.init. */
function scriptMcp(respond: (call: number, c: McpCall) => Response): { calls: McpCall[]; inits: (RequestInit | undefined)[] } {
  const calls: McpCall[] = []
  const inits: (RequestInit | undefined)[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    inits.push(init)
    if (url.includes("/api/query/sql")) {
      // markdown с data-av-id — контракт avIdOfBlock (REST-лега read_database_cells)
      return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] }), { status: 200 })
    }
    if (!url.includes("/mcp")) throw new Error(`mock: неожиданный путь ${url}`)
    const payload = JSON.parse(String(init?.body ?? "{}")) as { method?: string, params?: { arguments?: { page?: number } } }
    const headers = (init?.headers ?? {}) as Record<string, string>
    const sessionId = Object.entries(headers).find(([h]) => h.toLowerCase() === "mcp-session-id")?.[1] ?? ""
    const call = calls.length
    const info: McpCall = {
      method: payload.method ?? "?",
      hasSession: sessionId !== "",
      sessionId,
      page: payload.params?.arguments?.page ?? 0,
    }
    calls.push(info)
    return respond(call, info)
  }) as typeof fetch
  return { calls, inits }
}

const jsonrpc = (result: unknown, sessionId?: string): Response =>
  new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result }), {
    status: 200,
    headers: sessionId !== undefined ? { "mcp-session-id": sessionId } : {},
  })

const sse = (result: unknown): Response => {
  const payload = JSON.stringify({ jsonrpc: "2.0", id: 1, result })
  return new Response(`event: message\ndata: ${payload}\n\n`, { status: 200 })
}

/** Скрипт render-страниц (контракт пагинации ядра: page 1 несёт rowCount,
 *  дальше пустая страница терминирует выборку адаптера); формат ответа
 *  задаёт wrap (jsonrpc / sse). mcpRpc-тестам не нужна полнота доски —
 *  одна строка на page 1 достаточно. */
const pagedRender = (wrap: (result: unknown) => Response): ((call: number) => Response) => {
  let page = 0
  return (_call) => {
    page++
    return page === 1
      ? wrap({ structuredContent: { view: { rows: [{ id: "rowX", cells: [] }], rowCount: 1 } } })
      : wrap({ structuredContent: { view: { rows: [] } } })
  }
}

/** Handshake mcp-сессии: initialize → initialized; tools/call — по скрипту. */
const handshake = (sessionId: string, toolResponder: (call: number, c: McpCall) => Response) =>
  (call: number, c: McpCall): Response => {
    if (c.method === "initialize") return jsonrpc({}, sessionId)
    if (c.method === "notifications/initialized") return new Response("", { status: 200 })
    return toolResponder(call, c)
  }

const readCells = (): Promise<unknown> =>
  mcp.tool("read_database_cells", { docId: "doc1", databaseBlockId: "av1" })

beforeEach(() => {
  mcp.__test.expireSession()
})

afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("mcpRpc: протухшая сессия (рестарт ядра)", () => {
  test("404 при сессии в заголовке — один re-handshake, вызов проходит", async () => {
    // первый tools/call с сессией отклоняется 404 → reconnect → успех
    const render = pagedRender(jsonrpc)
    const { calls } = scriptMcp(handshake("s1", (call) => (call === 2 ? new Response("session not found", { status: 404 }) : render(call))))
    const r = (await readCells()) as { rows?: { rowBlockId?: string }[] }
    expect(r.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
    expect(calls.map((c) => c.method)).toEqual(["initialize", "notifications/initialized", "tools/call", "initialize", "notifications/initialized", "tools/call"])
    expect(calls[3]?.hasSession).toBe(false) // reconnect идёт с чистой сессией
  })

  test("второй 404 подряд — описательная ошибка, без цикла", async () => {
    // каждый tools/call с сессией отклоняется; reconnect выполняется один раз
    const { calls } = scriptMcp(handshake("s1", () => new Response("session not found", { status: 404 })))
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err).not.toBeNull()
    expect(err?.message).toContain("HTTP 404")
    expect(err?.constructor.name).not.toBe("SyntaxError")
    expect(calls.filter((c) => c.method === "initialize").length).toBe(2) // ровно один reconnect
  })

  test("404 без сессии в заголовке — ошибка без ретрая", async () => {
    // initialize отвечает без mcp-session-id → tools/call уходит без заголовка
    const { calls } = scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({})
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response("session not found", { status: 404 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("HTTP 404")
    expect(calls.map((c) => c.method)).toEqual(["initialize", "notifications/initialized", "tools/call"])
  })

  test("параллельные вызовы при экспирации — один recovery на пачку, оба проходят", async () => {
    // спавн-волна фабрики бьёт по ядру пачкой: все вызовы со старой сессией
    // получают 404 одновременно — handshake один, оба ретрая идут с новой
    let initialized = 0
    const rowPage = (page: number): Response =>
      jsonrpc(page === 1
        ? { structuredContent: { data: { view: { rows: [{ id: "rowX", cells: [] }], rowCount: 1 } } } }
        : { structuredContent: { data: { view: { rows: [] } } } })
    const { calls } = scriptMcp((_call, c) => {
      if (c.method === "initialize") {
        initialized++
        return jsonrpc({}, `s${initialized}`)
      }
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return c.sessionId === "s1" ? new Response("session not found", { status: 404 }) : rowPage(c.page)
    })

    const [a, b] = await Promise.all([
      readCells() as Promise<{ rows?: { rowBlockId?: string }[] }>,
      readCells() as Promise<{ rows?: { rowBlockId?: string }[] }>,
    ])

    expect(a.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
    expect(b.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
    // один стартовый handshake + один recovery — не по handshake на вызов
    expect(calls.filter((c) => c.method === "initialize")).toHaveLength(2)
    expect(calls.filter((c) => c.method === "tools/call" && c.sessionId === "s2").length).toBeGreaterThanOrEqual(2)
  })

  test("kernel-вызовы несут AbortSignal — зависший TCP не держит тик", async () => {
    // контракт таймаута: и REST-лега, и /mcp уходят с signal
    const { inits } = scriptMcp(handshake("s1", pagedRender(jsonrpc)))
    await readCells()
    for (const init of inits) {
      expect(init?.signal).toBeInstanceOf(AbortSignal)
    }
  })
})

describe("mcpRpc: честные ошибки вместо молчаливых пустых результатов", () => {
  test("kernel-конверт 401 «Auth failed» — throw, не пустой {}", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response(JSON.stringify({ code: -1, msg: "Auth failed [session]" }), { status: 401 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("kernel code=-1")
    expect(err?.message).toContain("Auth failed")
  })

  test("не-JSON тело при 200 (мусор шлюза) — ошибка со сниппетом", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response("<html>gateway oops</html>", { status: 200 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("не-JSON")
    expect(err?.message).toContain("gateway oops")
  })

  test("jsonrpc error envelope — как раньше, с текстом ошибки", async () => {
    scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, error: { code: 0, message: "method invalid" } }), { status: 200 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err?.message).toContain("method invalid")
  })

  test("успешный вызов в SSE-обёртке (data:-строки) — парсится как раньше", async () => {
    const render = pagedRender(sse)
    scriptMcp(handshake("s1", (_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s1")
      if (c.method === "notifications/initialized") return new Response("", { status: 200 })
      return render(0)
    }))
    const r = (await readCells()) as { rows?: { rowBlockId?: string }[] }
    expect(r.rows?.map((row) => row.rowBlockId)).toEqual(["rowX"])
  })
})


describe("mcpRpc: вложенная экспирация во время recovery-handshake", () => {
  test("404 на notifications/initialized — описательная ошибка, не вечное ожидание", async () => {
    // initialize ок (выдал s2), но initialized с новой сессией отклонён —
    // ядро умер прямо посреди handshake; recovery не должен звать сам себя
    const { calls } = scriptMcp((_call, c) => {
      if (c.method === "initialize") return jsonrpc({}, "s2")
      if (c.method === "notifications/initialized") return new Response("session not found", { status: 404 })
      return new Response("??", { status: 200 })
    })
    const err = await readCells().then(() => null, (e: Error) => e)
    expect(err).not.toBeNull()
    expect(err?.message).toContain("во время recovery-handshake")
    // никакого повторного initialize — цикл не завёлся
    expect(calls.filter((c) => c.method === "initialize")).toHaveLength(1)
  })
})

describe("parseKernelTimeoutMs: валидация env UWUWU_KERNEL_TIMEOUT_MS", () => {
  test("число-строка — миллисекунды", () => {
    expect(__test.parseKernelTimeoutMs("30000")).toBe(30000)
  })
  test("отсутствие env — дефолт 15 s; пустая строка — отказ (Number('')=0)", () => {
    expect(__test.parseKernelTimeoutMs(undefined)).toBe(15_000)
    expect(() => __test.parseKernelTimeoutMs("")).toThrow(/положительное число/)
    expect(() => __test.parseKernelTimeoutMs("   ")).toThrow(/положительное число/)
  })
  test("пустое значение env (Number('')=0) — отказ, а не мгновенный таймаут", () => {
    expect(() => __test.parseKernelTimeoutMs("")).toThrow(/положительное число/)
  })
  test("не-число и ноль/отрицательное — отказ", () => {
    expect(() => __test.parseKernelTimeoutMs("abc")).toThrow(/положительное число/)
    expect(() => __test.parseKernelTimeoutMs("0")).toThrow(/положительное число/)
    expect(() => __test.parseKernelTimeoutMs("-5")).toThrow(/положительное число/)
  })
})

describe("KernelUnavailableError: транспорт ядра отделён от ответа ядра", () => {
  const originalSetTimeout = globalThis.setTimeout
  afterEach(() => {
    globalThis.fetch = originalFetch
    globalThis.setTimeout = originalSetTimeout
  })

  /** fetch всегда падает транспортом; setTimeout без ожидания — ретраи post()
   *  не растягивают тест (4 попытки с бэкоффом 0.7/1.4/2.1/2.8 с). */
  const alwaysDown = (): { calls: number } => {
    const state = { calls: 0 }
    globalThis.setTimeout = ((fn: () => void) => { fn(); return 0 as unknown as ReturnType<typeof setTimeout> }) as typeof setTimeout
    globalThis.fetch = (async () => {
      state.calls++
      throw new TypeError("fetch failed")
    }) as typeof fetch
    return state
  }

  const errOf = (p: Promise<unknown>): Promise<unknown> => p.then(() => null, (e: unknown) => e)

  test("ядро лежит при handshake — типизированная ошибка с контекстом метода", async () => {
    alwaysDown()
    const err = await errOf(mcp.connect())
    expect(err instanceof mcp.KernelUnavailableError).toBe(true)
    expect((err as Error).message).toContain("initialize")
    expect((err as Error).message).toContain("ядро недоступно")
  })

  test("REST-лега (sql) при лежащем ядре — тот же класс, 4 ретрая сохранены", async () => {
    const probe = alwaysDown()
    const err = await errOf(mcp.tool("find_doc_by_title", { title: "x" }))
    expect(err instanceof mcp.KernelUnavailableError).toBe(true)
    expect((err as Error).message).toContain("/api/query/sql")
    expect(probe.calls).toBe(4)
  })

  test("транспорт упал на tools/call при живой сессии — одна попытка, без retry-цикла", async () => {
    let mcpCalls = 0
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input).includes("/api/query/sql")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] }), { status: 200 })
      }
      const payload = JSON.parse(String(init?.body ?? "{}")) as { method?: string }
      if (payload.method === "initialize") return jsonrpc({}, "sid-t")
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      mcpCalls++
      throw new TypeError("fetch failed")
    }) as typeof fetch
    const err = await errOf(readCells())
    expect(err instanceof mcp.KernelUnavailableError).toBe(true)
    expect((err as Error).message).toContain("tools/call")
    expect(mcpCalls).toBe(1)
  })

  test("404 протухшей сессии → recovery при лежащем ядре — отказ без зависания и цикла", async () => {
    let n = 0
    let kernelDown = false
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      n++
      if (String(input).includes("/api/query/sql")) {
        return new Response(JSON.stringify({ code: 0, msg: "", data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }] }), { status: 200 })
      }
      if (kernelDown) throw new TypeError("fetch failed") // recovery-рука при лежащем ядре
      const payload = JSON.parse(String(init?.body ?? "{}")) as { method?: string }
      if (payload.method === "initialize") return jsonrpc({}, "sid-k")
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      // tools/call: первый проход успешен (сессия жива), второй — ядро
      // перезапустилось и легло: 404 на протухшей сессии — последний вздох
      if (n === 4) return sse({ structuredContent: { view: { rows: [{ id: "rowX", cells: [] }], rowCount: 1 } } })
      if (n === 5) return sse({ structuredContent: { view: { rows: [] } } })
      kernelDown = true
      return new Response("session not found", { status: 404 })
    }) as typeof fetch
    await readCells() // сессия поднята, первый вызов прошёл
    const err = await errOf(readCells())
    expect(err instanceof mcp.KernelUnavailableError).toBe(true)
    expect((err as Error).message).toContain("initialize")
    expect((err as Error).message).toContain("ядро недоступно")
    expect(n).toBe(7) // 4 (первый проход) + sql + 404 + один initialize recovery
  })
})
