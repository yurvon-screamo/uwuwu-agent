// lib/compact.ts — автокомпакт вечных сессий: чистая матрица решений +
// REST-клей нативного эндпоинта opencode POST /session/{id}/compact (2.2.6,
// живые пробы задачи 20261105153731-s6e27f6). Компакт, admitted по idle-сессии,
// сам будит агент-луп (~26с до idle-маркера); маркер успеха — сообщение
// type:"compaction" в /message. Потребители — hub (триаж) и cron (вечные
// мастера custom-agent): оба отдают свою rest-функцию и адаптируют probe
// под свою семантику занятости, оркестрация тика — compactTick.

/** Использование токенов поворота (Session.Message.Assistant.tokens, 2.2.6). */
export interface TokenUsage {
  input: number
  output: number
  reasoning: number
  cache: { read: number; write: number }
}

/** Минимальная форма сообщения /message (desc-список) для скана токенов;
 *  полные интерфейсы SessionMsg демонов структурно совместимы. */
export interface CompactMsg {
  type?: string
  tokens?: TokenUsage
}

/** Политика автокомпакта: стрелять нативным compact по idle-сессии,
 *  когда контекст раздут ≥ minContextPct% лимита модели, не чаще cooldownMs. */
export interface CompactPolicy {
  idleMs: number
  minContextPct: number
  cooldownMs: number
}

/** Сумма токенов поворота = занятость окна контекста (то же, что TUI). */
export function contextTokens(t: TokenUsage): number {
  return t.input + t.output + t.reasoning + t.cache.read + t.cache.write
}

/** Токены свежайшего assistant-сообщения desc-списка: user/compaction/idle
 *  токенов не несут — сканируются мимо. null — assistant с tokens нет
 *  (пустая сессия — компактировать нечего). */
export function newestAssistantTokens(msgs: readonly CompactMsg[]): TokenUsage | null {
  for (const m of msgs) {
    if (m.type !== "assistant") continue
    const t = m.tokens
    if (t === undefined || typeof t.input !== "number") continue
    return t
  }
  return null
}

/** Занятость контекста в процентах лимита модели; null — не измерить
 *  (лимит неизвестен / токенов нет). */
export function contextUsagePct(tokens: TokenUsage | null, limitContext: number | null): number | null {
  if (tokens === null || limitContext === null || !Number.isFinite(limitContext) || limitContext <= 0) return null
  return (contextTokens(tokens) / limitContext) * 100
}

export interface CompactFacts {
  /** ms простоя сессии; null — граница вызывающего не подтвердила idle
   *  (занята/сбой чтения) — не стреляем. */
  idleMs: number | null
  /** занятость контекста, % лимита модели; null — не измерить. */
  usagePct: number | null
  /** семантика занятости демона (run/firing/очередь); busy исключает fire. */
  busy: boolean
  /** метка последней admission-попытки (0 — нет); окно коулдауна глушит. */
  lastCompactAt: number
  now: number
}

export interface CompactVerdict {
  fire: boolean
  reason: string
}

/** Вся матрица решения в одном чистом месте; reason — для лога отказа/выстрела. */
export function compactVerdict(facts: CompactFacts, policy: CompactPolicy): CompactVerdict {
  if (facts.busy) return { fire: false, reason: "сессия занята" }
  if (facts.idleMs === null) return { fire: false, reason: "idle не подтверждён" }
  if (facts.idleMs < policy.idleMs) return { fire: false, reason: `idle ${Math.round(facts.idleMs / 60_000)}м < ${Math.round(policy.idleMs / 60_000)}м` }
  if (facts.usagePct === null) return { fire: false, reason: "контекст не измерен" }
  if (facts.usagePct < policy.minContextPct) return { fire: false, reason: `контекст ${Math.round(facts.usagePct)}% < ${policy.minContextPct}%` }
  if (facts.now - facts.lastCompactAt < policy.cooldownMs) return { fire: false, reason: "коулдаун прошлой admission" }
  return { fire: true, reason: `idle ${Math.round(facts.idleMs / 3_600_000 * 10) / 10}ч, контекст ${Math.round(facts.usagePct)}%` }
}

// ——— REST-клей (rest — restCall демона: path/init → unparsed body) ———

export type RestCall = (path: string, init?: { method?: string; body?: unknown }) => Promise<unknown>

const isRecord = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object"

/** POST нативного компакта: durably admits compaction request — сам будит
 *  idle-луп, результат (compaction-сообщение) асинхронный. false — сбой. */
export async function sessionCompact(rest: RestCall, sessionID: string): Promise<boolean> {
  try {
    await rest(`/session/${sessionID}/compact`, { method: "POST", body: {} })
    return true
  } catch {
    return false
  }
}

/** Лимит окна контекста модели: GET /api/model, матч providerID+modelID.
 *  Кэш инжектируется (дефолт — процессный Map): лимиты статичны в рамках
 *  запуска, тесты подставляют свой. Кэшуются ТОЛЬКО успешные резолвы —
 *  включая «модели нет/limit пуст» (перманентный факт, API-drift); transient-
 *  сбой транспорта не кэшируется — ретрай следующим тиком (иначе первый
 *  замер на отказе опенкода тихо отключил бы компакт до рестарта демона).
 *  null — модель не резолвится / сбой. */
export async function modelContextLimit(
  rest: RestCall,
  providerID: string,
  modelID: string,
  cache: Map<string, number | null> = MODEL_LIMIT_CACHE,
): Promise<number | null> {
  const key = `${providerID}/${modelID}`
  const cached = cache.get(key)
  if (cached !== undefined) return cached
  let raw: unknown
  try {
    // NB: путь БЕЗ /api-префикса — restCall демона строит `${base}/api${path}`
    // (баг-репорт тестировщика 10.11: rest("/api/model") давал живой
    // GET /api/api/model → 404, компакт молчал вживую навсегда)
    raw = await rest("/model")
  } catch {
    return null // transient: не кэшируем — ретрай следующим тиком
  }
  const items = Array.isArray(raw) ? raw : (isRecord(raw) && Array.isArray(raw.data) ? raw.data : [])
  let resolved: number | null = null
  for (const item of items) {
    if (!isRecord(item) || item.providerID !== providerID || item.modelID !== modelID) continue
    const limit = isRecord(item.limit) ? item.limit : {}
    const ctx = limit.context
    resolved = typeof ctx === "number" && Number.isFinite(ctx) && ctx > 0 ? ctx : null
    break
  }
  cache.set(key, resolved)
  return resolved
}

const MODEL_LIMIT_CACHE = new Map<string, number | null>()

export interface SessionProbe {
  /** false — сессия пропала (404/400); остальные поля невалидны. */
  alive: boolean
  /** time.idle сессии (ms); границу (стухший idle хода) применяет вызывающий.
   *  null — метки нет (свежая сессия) / сбой чтения. */
  idleAt: number | null
  /** занятость контекста, % лимита модели сессии; null — не измерить. */
  usagePct: number | null
}

/** Проба сессии: GET /session (idle + модель) + GET /message (токены свежайшего
 *  assistant-поворота) против лимита модели. Сбои чтения — null-поля, не бросок
 *  (тик демона переживает деградацию opencode штатным пропуском). */
export async function compactProbe(rest: RestCall, sessionID: string): Promise<SessionProbe> {
  let idleAt: number | null = null
  let usagePct: number | null = null
  let alive = true
  try {
    const s = await rest(`/session/${sessionID}`)
    if (!isRecord(s)) throw new Error("форма ответа сессии незнакома")
    const idle = (isRecord(s.time) ? s.time.idle : undefined)
    if (typeof idle === "number" && Number.isFinite(idle) && idle > 0) idleAt = idle
    const model = isRecord(s.model) ? s.model : {}
    const providerID = model.providerID
    const modelID = model.id
    if (typeof providerID === "string" && typeof modelID === "string") {
      const msgs = await rest(`/session/${sessionID}/message?limit=10&order=desc`)
      const list = Array.isArray(msgs) ? msgs : (isRecord(msgs) && Array.isArray(msgs.data) ? msgs.data : [])
      const tokens = newestAssistantTokens(list as CompactMsg[])
      usagePct = contextUsagePct(tokens, await modelContextLimit(rest, providerID, modelID))
    }
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) alive = false
  }
  return { alive, idleAt, usagePct }
}

// ——— оркестрация тика (общая для hub и cron; зависимости инжектируются) ———

export interface CompactDeps {
  /** id вечной сессии демона; undefined — нечего компактить. */
  sessionID(): string | undefined
  /** семантика занятости демона (hub: run/firing/очередь; cron: pendingRun). */
  busy(): boolean
  /** проба сессии: адаптация compactProbe под границы демона. */
  probe(sessionID: string): Promise<{ idleMs: number | null; usagePct: number | null }>
  lastCompactAt(): number
  /** персист метки admission ДО fire (фиксация ревью: обратный порядок
   *  оставляет окно пере-admission при крае между POST и save). */
  persist(now: number): void
  /** POST нативного компакта (sessionCompact на rest демона). */
  fire(sessionID: string): Promise<boolean>
  policy: CompactPolicy
  log(m: string): void
}

/** Один тик автокомпакта: гейты → вердикт → персист метки → POST. Отказ
 *  вердикта — тихо (тик каждые 5–10 мин, шум в лог не льём); выстрел и сбой
 *  POST — в лог. Сбой probe долетает как null-поля verdict'а — тоже тихо. */
export async function compactTick(deps: CompactDeps): Promise<void> {
  const sessionID = deps.sessionID()
  if (sessionID === undefined || deps.busy()) return
  const now = Date.now()
  const probe = await deps.probe(sessionID)
  const verdict = compactVerdict(
    { idleMs: probe.idleMs, usagePct: probe.usagePct, busy: false, lastCompactAt: deps.lastCompactAt(), now },
    deps.policy,
  )
  if (!verdict.fire) return
  deps.persist(now)
  const ok = await deps.fire(sessionID)
  deps.log(ok
    ? `compact: заявка приземлена (${sessionID}; ${verdict.reason})`
    : `compact: POST не прошёл (${verdict.reason}) — ретрай после коулдауна`)
}
