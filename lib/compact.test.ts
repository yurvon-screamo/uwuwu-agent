// compact.test.ts — тесты автокомпакта вечных сессий: чистая матрица вердикта,
// скан токенов, REST-клей на мок-rest, оркестрация compactTick (гейты, порядок
// «персист до POST», коулдаун). bun test, без сети.
import { test, expect } from "bun:test"
import {
  compactTick, compactVerdict, contextTokens, contextUsagePct, compactProbe,
  modelContextLimit, newestAssistantTokens, sessionCompact,
  type CompactDeps, type CompactMsg, type CompactPolicy, type TokenUsage,
} from "./compact"

const POLICY: CompactPolicy = { idleMs: 4 * 3_600_000, minContextPct: 40, cooldownMs: 3_600_000 }
const HOUR = 3_600_000

const usage = (total: number): TokenUsage => ({
  input: total, output: 0, reasoning: 0, cache: { read: 0, write: 0 },
})

// ——— contextTokens / contextUsagePct ———

test("contextTokens: сумма всех компонент поворота", () => {
  expect(contextTokens({ input: 10, output: 2, reasoning: 3, cache: { read: 100, write: 5 } })).toBe(120)
})

test("contextUsagePct: доля лимита; null на нулевом/кривом лимите и без токенов", () => {
  expect(contextUsagePct(usage(400_000), 1_000_000)).toBe(40)
  expect(contextUsagePct(usage(400_000), 0)).toBeNull()
  expect(contextUsagePct(usage(400_000), null)).toBeNull()
  expect(contextUsagePct(null, 1_000_000)).toBeNull()
})

// ——— newestAssistantTokens ———

const m = (over: Partial<CompactMsg>): CompactMsg => ({ type: "user", ...over })

test("newestAssistantTokens: первый assistant с tokens в desc-списке; прочие типы мимо", () => {
  const msgs: CompactMsg[] = [
    m({ type: "idle" }),                        // маркер простоя — мимо
    m({ type: "compaction" }),                  // компакция — мимо
    m({ type: "assistant", tokens: usage(300) }),
    m({ type: "user" }),                        // токенов не несёт
    m({ type: "assistant", tokens: usage(900) }), // старьё — не берём
  ]
  expect(newestAssistantTokens(msgs)).toEqual(usage(300))
  expect(newestAssistantTokens([m({ type: "user" })])).toBeNull()
  expect(newestAssistantTokens([])).toBeNull()
  expect(newestAssistantTokens([m({ type: "assistant" })])).toBeNull() // assistant без tokens
})

// ——— compactVerdict: матрица решения ———

const okFacts = { idleMs: 5 * HOUR, usagePct: 45, busy: false, lastCompactAt: 0, now: 10 * HOUR }

test("compactVerdict: все условия сходятся — fire с человекочитаемым reason", () => {
  const v = compactVerdict(okFacts, POLICY)
  expect(v.fire).toBe(true)
  expect(v.reason).toContain("idle")
  expect(v.reason).toContain("%")
})

test("compactVerdict: busy, неподтверждённый idle и недобор idle — отказ", () => {
  expect(compactVerdict({ ...okFacts, busy: true }, POLICY)).toEqual({ fire: false, reason: "сессия занята" })
  expect(compactVerdict({ ...okFacts, idleMs: null }, POLICY)).toEqual({ fire: false, reason: "idle не подтверждён" })
  const under = compactVerdict({ ...okFacts, idleMs: 2 * HOUR }, POLICY)
  expect(under.fire).toBe(false)
  expect(under.reason).toContain("<")
})

test("compactVerdict: недобор процента, неизмеренный контекст и коулдаун — отказ", () => {
  const under = compactVerdict({ ...okFacts, usagePct: 12 }, POLICY)
  expect(under.fire).toBe(false)
  expect(under.reason).toContain("12%")
  expect(compactVerdict({ ...okFacts, usagePct: null }, POLICY)).toEqual({ fire: false, reason: "контекст не измерен" })
  const cooldown = compactVerdict({ ...okFacts, lastCompactAt: 10 * HOUR - 1 }, POLICY)
  expect(cooldown.fire).toBe(false)
  expect(cooldown.reason).toContain("коулдаун")
  expect(compactVerdict({ ...okFacts, lastCompactAt: 10 * HOUR - POLICY.cooldownMs }, POLICY).fire).toBe(true)
})

// ——— sessionCompact / modelContextLimit: REST-клей на мок-rest ———

test("sessionCompact: POST на /compact с пустым телом; сбой — false", async () => {
  const calls: { path: string; init?: { method?: string; body?: unknown } }[] = []
  const ok = await sessionCompact(async (path, init) => {
    calls.push({ path, init })
  }, "ses_x")
  expect(ok).toBe(true)
  expect(calls[0]?.path).toBe("/session/ses_x/compact")
  expect(calls[0]?.init?.method).toBe("POST")
  const fail = await sessionCompact(async () => { throw new Error("boom") }, "ses_x")
  expect(fail).toBe(false)
})

const modelRow = (providerID: string, modelID: string, context: number): unknown => ({
  providerID, modelID, limit: { context, output: 131_072 },
})

test("modelContextLimit: матч по providerID+modelID; нет модели / пустой limit — null", async () => {
  const cache = new Map<string, number | null>()
  const rest = async (path: string): Promise<unknown> => {
    expect(path).toBe("/api/model")
    return { data: [modelRow("prov", "m1", 1_000_000), modelRow("prov", "m2", 0)] }
  }
  expect(await modelContextLimit(rest, "prov", "m1", cache)).toBe(1_000_000)
  expect(await modelContextLimit(rest, "prov", "m2", cache)).toBeNull()
  expect(await modelContextLimit(rest, "prov", "missing", cache)).toBeNull()
})

test("modelContextLimit: кэш — второй вызов не ходит в rest; сбой rest кэшируется null", async () => {
  const cache = new Map<string, number | null>()
  let fetches = 0
  const rest = async (): Promise<unknown> => {
    fetches += 1
    return [modelRow("prov", "m1", 500_000)]
  }
  expect(await modelContextLimit(rest, "prov", "m1", cache)).toBe(500_000)
  expect(await modelContextLimit(rest, "prov", "m1", cache)).toBe(500_000)
  expect(fetches).toBe(1)
  const failing = async (): Promise<unknown> => { throw new Error("down") }
  expect(await modelContextLimit(failing, "prov", "m2", cache)).toBeNull()
  expect(await modelContextLimit(failing, "prov", "m2", cache)).toBeNull() // из кэша
})

// ——— compactProbe: форма живого сервера, сбои — null-поля ———

const sessionShape = (over: Record<string, unknown>): unknown => ({
  id: "ses_x", outcome: "succeeded",
  time: { created: 1, updated: 2, idle: 3 },
  model: { providerID: "prov", id: "m1" },
  ...over,
})

function scriptRest(session: unknown, messages?: unknown): { rest: (path: string) => Promise<unknown>; calls: string[] } {
  const calls: string[] = []
  return {
    calls,
    rest: async (path: string) => {
      calls.push(path)
      if (path.includes("/message")) {
        if (messages === undefined) throw new Error("messages недоступны")
        return { data: messages }
      }
      if (path === "/api/model") return [modelRow("prov", "m1", 1_000_000)]
      if (typeof session === "number") throw Object.assign(new Error("gone"), { status: session })
      return session
    },
  }
}

test("compactProbe: idle+модель+токены читаются, pct против лимита модели", async () => {
  const { rest, calls } = scriptRest(sessionShape({}), [
    m({ type: "assistant", tokens: usage(400_000) }),
    m({ type: "user" }),
  ])
  const probe = await compactProbe(rest, "ses_x")
  expect(probe).toEqual({ alive: true, idleAt: 3, usagePct: 40 })
  expect(calls.some((c) => c.includes("/message?limit=10&order=desc"))).toBe(true)
})

test("compactProbe: сессия пропала — alive false; кривые поля — null-поля, не бросок", async () => {
  expect(await compactProbe(scriptRest(404).rest, "ses_x")).toEqual({ alive: false, idleAt: null, usagePct: null })
  expect(await compactProbe(scriptRest(sessionShape({ time: {} })).rest, "ses_x"))
    .toEqual({ alive: true, idleAt: null, usagePct: null })
  const broken = scriptRest(sessionShape({ model: {} }), [m({ type: "assistant", tokens: usage(1) })])
  expect(await compactProbe(broken.rest, "ses_x")).toEqual({ alive: true, idleAt: 3, usagePct: null })
  const down = scriptRest(sessionShape({}), [m({ type: "assistant", tokens: usage(1) })])
  down.calls // messages-замокан в отказ
  const probe = await compactProbe((path) => path.includes("/message")
    ? Promise.reject(new Error("boom"))
    : down.rest(path), "ses_x")
  expect(probe.alive).toBe(true)
  expect(probe.usagePct).toBeNull()
})

// ——— compactTick: оркестрация (гейты, порядок «персист до POST», коулдаун) ———

interface FakeDeps {
  deps: CompactDeps
  calls: string[]
}

function fakeDeps(over: {
  sessionID?: string | null
  busy?: boolean
  idleMs?: number | null
  usagePct?: number | null
  lastCompactAt?: number
  fireFails?: boolean
  policy?: CompactPolicy
}): FakeDeps {
  const calls: string[] = []
  const deps: CompactDeps = {
    sessionID: () => over.sessionID === null ? undefined : (over.sessionID ?? "ses_x"),
    busy: () => over.busy ?? false,
    probe: async () => ({
      idleMs: over.idleMs !== undefined ? over.idleMs : 5 * HOUR,
      usagePct: over.usagePct !== undefined ? over.usagePct : 45,
    }),
    lastCompactAt: () => over.lastCompactAt ?? 0,
    persist: (now) => calls.push(`persist:${now}`),
    fire: async () => {
      calls.push("fire")
      return !(over.fireFails ?? false)
    },
    policy: over.policy ?? POLICY,
    log: (msg) => calls.push(`log:${msg}`),
  }
  return { deps, calls }
}

test("compactTick: выстрел — персист метки ДО POST, факт в лог", async () => {
  const { deps, calls } = fakeDeps({})
  await compactTick(deps)
  expect(calls.filter((c) => c.startsWith("persist") || c === "fire"))
    .toEqual([expect.stringMatching(/^persist:/), "fire"])
  expect(calls.some((c) => c.startsWith("log:compact: заявка приземлена"))).toBe(true)
})

test("compactTick: гейты — нет сессии и busy тихо, без probe и POST", async () => {
  const none = fakeDeps({ sessionID: null })
  await compactTick(none.deps)
  expect(none.calls).toEqual([])
  const busy = fakeDeps({ busy: true })
  await compactTick(busy.deps)
  expect(busy.calls).toEqual([])
})

test("compactTick: вердикт-отказ — тихо, persist и POST не звались", async () => {
  for (const over of [{ idleMs: HOUR }, { usagePct: 5 }, { usagePct: null }, { idleMs: null }]) {
    const { deps, calls } = fakeDeps(over)
    await compactTick(deps)
    expect(calls.filter((c) => c === "fire" || c.startsWith("persist"))).toEqual([])
  }
})

test("compactTick: коулдаун прошлой admission глушит; сбой POST — лог ретрая", async () => {
  const now = Date.now()
  const cooldown = fakeDeps({ lastCompactAt: now - 1 })
  await compactTick(cooldown.deps)
  expect(cooldown.calls).toEqual([])

  const passed = fakeDeps({ lastCompactAt: now - POLICY.cooldownMs - 1000, fireFails: true })
  await compactTick(passed.deps)
  expect(passed.calls.some((c) => c.startsWith("log:compact: POST не прошёл"))).toBe(true)
})
