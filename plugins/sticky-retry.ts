import { Plugin } from "@opencode/plugin"
import { appendFileSync, mkdirSync } from "node:fs"
import { dirname } from "node:path"

// sticky-retry — infinite provider retries for OpenCode V2.
//
// Port of the V1 plugin opencode-sticky-retry (ronanhansel, MIT) to the V2
// plugin API. The V1 approach (wrapping globalThis.fetch) is impossible in
// V2: provider requests bypass a runtime-patched fetch (verified
// empirically). Instead this plugin:
//
//   1. rewrites every model request baseURL (session.hook "model.request",
//      a documented mutable field) to a tiny in-process HTTP forwarder
//      (Bun.serve on 127.0.0.1), and
//   2. the forwarder retries transport errors, 408, 429 and 5xx forever,
//      passing everything else straight through, streaming responses back.
//   3. a session.hook("retry") fallback covers traffic that bypasses the
//      forwarder (websocket transports, packages ignoring baseURL, mid-
//      stream breaks after 200). OpenCode's built-in cap there is 5
//      attempts (measured) — retries use the same delay schedule.
//   4. auto-continue: OpenCode kills a model stream after ~480s without
//      content (SSE pings do not reset the watchdog, measured) and its
//      retry pipeline caps at 5 dispatches, so one provider outage is
//      absorbed autonomously for roughly an hour. When the session then
//      fails with a TRANSIENT error (network / rate limit / 5xx — never
//      deterministic 4xx), the plugin re-prompts the session itself and
//      the fight continues indefinitely. Completed tool calls are history
//      and never re-run; a user interrupt disables auto-continue for that
//      session until the next user prompt.
//
// Delays: 1, 2, 3, 5, 7, 10, 15 minutes, then every 15 minutes. A larger
// Retry-After header (seconds or HTTP-date) wins over the schedule
// (+1s buffer, capped at 30 min). A closed connection aborts the retry loop
// immediately (request.signal / stream cancel / enqueue failure — all
// verified). A session interrupt (Esc) does not close OpenCode's dispatch
// connection: the in-flight loop then ends with OpenCode's ~480s watchdog
// and auto-continue respects the interrupt.
//
// Relay semantics: the first upstream attempt happens before the response is
// committed, so non-retryable errors (400/401/404 ...) reach OpenCode with
// their true status. Only after a retryable failure does an SSE request get
// a committed 200 + text/event-stream response that interleaves SSE ping
// events while retrying — they keep both HTTP parties from killing an idle
// connection during long waits (OpenCode's content watchdog still kills the
// stream after ~480s; the auto-continue layer takes over from there).
// Non-SSE requests wait inside the handler and relay the final response
// truthfully (bounded by the server's pending-handler patience, which the
// E2E suite measures).
//
// Known limitations (deliberate):
//   - While the forwarder retries, the session looks busy; retry status is
//     only visible in the log file (LOG_PATH below).
//   - Request bodies are buffered once to make retries replayable; very
//     large streaming request bodies are not supported (LLM JSON is fine).
//   - A retried POST whose request actually reached the provider may be
//     billed twice — the standard tradeoff of every retry tool.
//   - The forwarder binds 127.0.0.1 with an ephemeral port and forwards to
//     any http/https URL encoded in its path: on a shared multi-user host
//     every local user could use it as an egress forwarder. It stores no
//     secrets itself.
//   - The forwarder lives for the server process lifetime (created once,
//     never stopped on plugin unload).
//   - Non-SSE requests relay through a pending handler: Bun.serve drops an
//     idle connection after 255s, so non-SSE retry pauses above ~4 minutes
//     end with a clean abort instead of a retried answer. Model calls are
//     SSE; this bound only matters for exotic non-streaming traffic.

const LOG_DIR = `${process.env.HOME ?? "."}/.local/share/opencode/log`
const LOG_PATH = `${LOG_DIR}/sticky-retry.log`

/** Delay schedule in ms: 1, 2, 3, 5, 7, 10, 15 min — then 15 min forever. */
export const SCHEDULE_MS = [60_000, 120_000, 180_000, 300_000, 420_000, 600_000, 900_000] as const
export const RETRY_AFTER_BUFFER_MS = 1_000
export const RETRY_AFTER_CAP_MS = 30 * 60_000
const KEEPALIVE_INTERVAL_MS = 30_000
/** OpenCode's silent-stream watchdog kills a legitimate model stream after
 *  ~480s, so a retrying proxy stream older than this cap is serving an
 *  abandoned connection (interrupted or orphaned session that never closed
 *  it). The check runs after each sleep, so the worst-case cleanup bound is
 *  this cap plus one full schedule pause (~35 minutes). */
const STREAM_MAX_LIFETIME_MS = 20 * 60_000
const PATH_PREFIX = "/s/"
const RETRYABLE_STATUSES = new Set([408, 429])

const log = (message: string): void => {
  try {
    appendFileSync(LOG_PATH, `${new Date().toISOString()} ${message}\n`)
  } catch {
    // logging must never break traffic
  }
}

const originAndPath = (url: string): string => {
  try {
    const parsed = new URL(url)
    return parsed.origin + parsed.pathname
  } catch {
    return url
  }
}

export const scheduleDelayMs = (attempt: number): number =>
  SCHEDULE_MS[Math.min(Math.max(attempt, 1), SCHEDULE_MS.length) - 1]

/** Parse Retry-After (delay-seconds or HTTP-date) into a delay in ms. */
export const parseRetryAfterMs = (
  header: string | null,
  now = Date.now(),
): number | undefined => {
  if (header === null || header.trim() === "") return undefined
  const trimmed = header.trim()
  const asNumber = Number(trimmed)
  if (Number.isFinite(asNumber)) return Math.max(0, asNumber * 1000)
  const asDate = Date.parse(trimmed)
  if (!Number.isNaN(asDate)) return Math.max(0, asDate - now)
  return undefined
}

export interface DecideInput {
  readonly status?: number
  readonly transportError?: boolean
  readonly retryAfterMs?: number
  readonly attempt: number
}

export interface DecideResult {
  readonly retry: boolean
  readonly delayMs: number
}

export const isRetryableStatus = (status: number): boolean =>
  RETRYABLE_STATUSES.has(status) || status >= 500

/** Retry policy: transport errors and 408/429/5xx retry with the schedule;
 *  a larger Retry-After wins (+buffer, capped). Everything else passes through. */
export const decide = (input: DecideInput): DecideResult => {
  if (input.transportError === true) {
    return { retry: true, delayMs: scheduleDelayMs(input.attempt) }
  }
  if (input.status === undefined || !isRetryableStatus(input.status)) {
    return { retry: false, delayMs: 0 }
  }
  const scheduled = scheduleDelayMs(input.attempt)
  const hinted =
    input.retryAfterMs === undefined
      ? scheduled
      : Math.min(Math.max(input.retryAfterMs + RETRY_AFTER_BUFFER_MS, scheduled), RETRY_AFTER_CAP_MS)
  return { retry: true, delayMs: hinted }
}

/** Rebuild the upstream URL: encoded target base + the path the provider SDK
 *  appended after the prefix. Concat semantics match what the SDK did with
 *  the original base; a trailing-slash base avoids a duplicate slash. */
export const buildUpstreamUrl = (targetBase: string, appendedPath: string): string => {
  if (appendedPath === "") return targetBase
  const base = targetBase.endsWith("/") && appendedPath.startsWith("/")
    ? targetBase.slice(0, -1)
    : targetBase
  return base + appendedPath
}

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))

/** Sleep that rejects immediately when the signal aborts (user interrupt /
 *  client disconnect) so retry loops never outlive their connection. */
export const abortableSleep = (ms: number, signal: AbortSignal): Promise<void> =>
  new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason ?? new DOMException("Aborted", "AbortError"))
      return
    }
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort)
      resolve()
    }, ms)
    const onAbort = () => {
      clearTimeout(timer)
      reject(signal.reason ?? new DOMException("Aborted", "AbortError"))
    }
    signal.addEventListener("abort", onAbort, { once: true })
  })

/** URLs inside logged error strings may carry query secrets — scrub them. */
export const scrubErrorText = (error: unknown): string =>
  String(error)
    .replace(/https?:\/\/\S+/g, "[url]")
    .slice(0, 160)

interface ProxyHandle {
  readonly port: number
}

/** Start the forwarder once per process (plugin setup runs per location). */
const startProxy = (): ProxyHandle | null => {
  try {
    mkdirSync(dirname(LOG_PATH), { recursive: true })
    const server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      idleTimeout: 255,
      fetch: (request) => proxyHandler(request),
    })
    log(`proxy listening on 127.0.0.1:${server.port}`)
    return { port: server.port }
  } catch (error) {
    log(`proxy failed to start — plugin inert, traffic unchanged (${String(error)})`)
    return null
  }
}

interface PreparedRequest {
  readonly upstreamUrl: string
  readonly displayUrl: string
  readonly method: string
  readonly headers: Headers
  readonly bodyBuffer: ArrayBuffer | undefined
  readonly abort: AbortController
}

const prepareUpstream = async (request: Request): Promise<PreparedRequest | null> => {
  const parsed = parseProxyPath(request.url)
  if (parsed === null) return null
  if (!/^https?:\/\//i.test(parsed.targetBase)) return null
  const upstreamUrl = buildUpstreamUrl(parsed.targetBase, parsed.appendedPath)
  const method = request.method.toUpperCase()
  const hasBody = method !== "GET" && method !== "HEAD"
  const headers = new Headers(request.headers)
  headers.delete("host")
  headers.delete("connection")
  const abort = new AbortController()
  // Client disconnect must stop the retry loop on BOTH relay paths; the SSE
  // stream cancel() below is the second, belt-and-suspenders trigger.
  const incomingSignal = (request as Request & { signal?: AbortSignal }).signal
  if (incomingSignal !== undefined) {
    const onDisconnect = () => abort.abort()
    incomingSignal.addEventListener("abort", onDisconnect, { once: true })
  }
  return {
    upstreamUrl,
    displayUrl: originAndPath(upstreamUrl),
    method,
    headers,
    bodyBuffer: hasBody ? await request.arrayBuffer() : undefined,
    abort,
  }
}

const fetchUpstream = (prepared: PreparedRequest): Promise<Response> =>
  fetch(prepared.upstreamUrl, {
    method: prepared.method,
    headers: prepared.headers,
    body: prepared.bodyBuffer === undefined ? undefined : prepared.bodyBuffer.slice(0),
    redirect: "manual",
    signal: prepared.abort.signal,
  })

const upstreamDecision = (upstream: Response, attempt: number) =>
  decide({
    status: upstream.status,
    retryAfterMs: parseRetryAfterMs(upstream.headers.get("retry-after")),
    attempt,
  })

const proxyHandler = async (request: Request): Promise<Response> => {
  const prepared = await prepareUpstream(request)
  if (prepared === null) {
    return new Response("sticky-retry: unknown path or scheme", { status: 404 })
  }

  const wantsSse = (request.headers.get("accept") ?? "").includes("text/event-stream")

  // First attempt before committing any response bytes: non-retryable
  // statuses must reach the caller with their true status and headers.
  let upstream: Response
  try {
    upstream = await fetchUpstream(prepared)
  } catch (error) {
    if (prepared.abort.signal.aborted) return new Response("aborted", { status: 499 })
    const decision = decide({ transportError: true, attempt: 1 })
    log(
      `retry transport="${scrubErrorText(error)}" attempt=1 next=${Math.round(decision.delayMs / 1000)}s url=${prepared.displayUrl}`,
    )
    return wantsSse
      ? retryingSseResponse(prepared, 2, decision.delayMs)
      : retryThenRelay(prepared, 2, decision.delayMs)
  }
  const decision = upstreamDecision(upstream, 1)
  if (!decision.retry) {
    log(`pass status=${upstream.status} attempt=1 url=${prepared.displayUrl}`)
    return relayResponse(upstream)
  }
  log(
    `retry status=${upstream.status} attempt=1 next=${Math.round(decision.delayMs / 1000)}s url=${prepared.displayUrl}`,
  )
  return wantsSse
    ? retryingSseResponse(prepared, 2, decision.delayMs)
    : retryThenRelay(prepared, 2, decision.delayMs)
}

/** Non-SSE path: the whole retry loop runs inside the handler, then the
 *  final response (whatever its status) is relayed truthfully. Every sleep
 *  is the delay of the PREVIOUS retry decision (schedule or Retry-After)
 *  and aborts as soon as the caller disconnects. */
const retryThenRelay = async (
  prepared: PreparedRequest,
  nextAttempt: number,
  firstDelayMs: number,
): Promise<Response> => {
  let pendingDelayMs = firstDelayMs
  const bornAt = Date.now()
  for (let attempt = nextAttempt; ; attempt += 1) {
    try {
      await abortableSleep(pendingDelayMs, prepared.abort.signal)
    } catch {
      return new Response("aborted", { status: 499 })
    }
    if (Date.now() - bornAt > STREAM_MAX_LIFETIME_MS) {
      log(`give-up stale-stream attempt=${attempt} url=${prepared.displayUrl}`)
      return new Response("sticky-retry: stream lifetime cap", { status: 504 })
    }
    try {
      const upstream = await fetchUpstream(prepared)
      const decision = upstreamDecision(upstream, attempt)
      if (!decision.retry) {
        log(`pass status=${upstream.status} attempt=${attempt} url=${prepared.displayUrl}`)
        return relayResponse(upstream)
      }
      pendingDelayMs = decision.delayMs
      log(
        `retry status=${upstream.status} attempt=${attempt} next=${Math.round(decision.delayMs / 1000)}s url=${prepared.displayUrl}`,
      )
    } catch (error) {
      if (prepared.abort.signal.aborted) return new Response("aborted", { status: 499 })
      pendingDelayMs = decide({ transportError: true, attempt }).delayMs
      log(
        `retry transport="${scrubErrorText(error)}" attempt=${attempt} next=${Math.round(pendingDelayMs / 1000)}s url=${prepared.displayUrl}`,
      )
    }
  }
}

/** SSE path: commit 200 + event-stream immediately, keep the connection
 *  warm with SSE ping events, retry until success, then pipe the body.
 *  Every sleep is the previous decision's delay (schedule or Retry-After)
 *  and aborts on client disconnect. A non-retryable status mid-loop ends
 *  the stream abruptly: the caller sees a broken stream and falls back to
 *  OpenCode's own retry pipeline. */
const retryingSseResponse = (
  prepared: PreparedRequest,
  nextAttempt: number,
  firstDelayMs: number,
): Response => {
  const encoder = new TextEncoder()
  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      // SSE comment lines don't reset OpenCode's silent-stream watchdog
      // (measured: streams with comment-only keepalives are killed after
      // ~480s), so keepalives are real ping events. eventsource-parser
      // accepts them and provider parsers ignore unknown event names.
      // Client-disconnect detection chain (all three verified in an
      // isolated Bun.serve experiment): the incoming request.signal aborts,
      // the stream cancel() fires, and the next enqueue throws. Note that
      // an OpenCode session interrupt does NOT close its dispatch
      // connection — the loop then ends with OpenCode's own ~480s
      // silent-stream watchdog, and auto-continue honors the interrupt.
      const keepalive = setInterval(() => {
        try {
          controller.enqueue(encoder.encode("event: ping\ndata: {\"type\":\"ping\"}\n\n"))
        } catch {
          // The client is gone: stop the keepalives AND abort the retry
          // loop, otherwise it would sleep and retry into a dead socket.
          clearInterval(keepalive)
          prepared.abort.abort()
        }
      }, KEEPALIVE_INTERVAL_MS)
      let pendingDelayMs = firstDelayMs
      const bornAt = Date.now()
      try {
        for (let attempt = nextAttempt; ; attempt += 1) {
          try {
            await abortableSleep(pendingDelayMs, prepared.abort.signal)
          } catch {
            return
          }
          if (Date.now() - bornAt > STREAM_MAX_LIFETIME_MS) {
            log(`give-up stale-stream attempt=${attempt} url=${prepared.displayUrl}`)
            return
          }
          try {
            const upstream = await fetchUpstream(prepared)
            const decision = upstreamDecision(upstream, attempt)
            if (!decision.retry) {
              if (upstream.ok) {
                log(`recovered status=${upstream.status} attempt=${attempt} url=${prepared.displayUrl}`)
                await pipeBody(upstream, controller)
                return
              }
              log(
                `give-up status=${upstream.status} attempt=${attempt} stream-closed url=${prepared.displayUrl}`,
              )
              return
            }
            pendingDelayMs = decision.delayMs
            log(
              `retry status=${upstream.status} attempt=${attempt} next=${Math.round(decision.delayMs / 1000)}s url=${prepared.displayUrl}`,
            )
          } catch (error) {
            if (prepared.abort.signal.aborted) return
            pendingDelayMs = decide({ transportError: true, attempt }).delayMs
            log(
              `retry transport="${scrubErrorText(error)}" attempt=${attempt} next=${Math.round(pendingDelayMs / 1000)}s url=${prepared.displayUrl}`,
            )
          }
        }
      } finally {
        clearInterval(keepalive)
        try {
          controller.close()
        } catch {
          // caller already gone
        }
      }
    },
    cancel() {
      prepared.abort.abort()
    },
  })
  return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } })
}

/** Bun's fetch auto-decompresses the upstream body while its headers still
 *  advertise the encoding — dropping both prevents a corrupted relay. */
export const relayResponse = (upstream: Response): Response => {
  const headers = new Headers(upstream.headers)
  headers.delete("content-encoding")
  headers.delete("content-length")
  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers,
  })
}

const pipeBody = async (
  upstream: Response,
  controller: ReadableStreamDefaultController<Uint8Array>,
): Promise<void> => {
  if (upstream.body === null) return
  const reader = upstream.body.getReader()
  for (;;) {
    const { done, value } = await reader.read()
    if (done) return
    if (value !== undefined) controller.enqueue(value)
  }
}

const parseProxyPath = (
  requestUrl: string,
): { targetBase: string; appendedPath: string } | null => {
  let parsed: URL
  try {
    parsed = new URL(requestUrl)
  } catch {
    return null
  }
  if (!parsed.pathname.startsWith(PATH_PREFIX)) return null
  const afterPrefix = parsed.pathname.slice(PATH_PREFIX.length)
  const nextSlash = afterPrefix.indexOf("/")
  if (nextSlash === -1) return null
  const encoded = afterPrefix.slice(0, nextSlash)
  const appendedPath = afterPrefix.slice(nextSlash) + parsed.search
  try {
    return { targetBase: decodeURIComponent(encoded), appendedPath }
  } catch {
    return null
  }
}

const PROXY_GUARD = Symbol.for("sticky-retry.proxy")
const AUTO_CONTINUE_GUARD = Symbol.for("sticky-retry.auto-continue")

const AUTO_CONTINUE_DEDUPE_MS = 15_000
const AUTO_CONTINUE_STATE_TTL_MS = 2 * 60 * 60_000
/** Safety bound: orphaned sessions (killed CLI, forgotten tabs) must not
 *  burn provider credits forever; 50 consecutive continues with the growing
 *  schedule covers roughly half a day of fast failures and multiple days
 *  of long outages. Raise it if you truly want unbounded auto-continue. */
const MAX_AUTO_CONTINUES = 50
const TRANSIENT_ERROR = /transport|rate|timeout|overload|server|stream|network|econn|fetch|invalid-output/i

interface SessionStateEntry {
  handledAt: number
  consecutive: number
  lastTouchedAt: number
  stopped: boolean
}

interface AutoContinueState {
  readonly sessions: Map<string, SessionStateEntry>
  /** Returns the entry for the session, creating and touching it if needed. */
  touch(sessionID: string): SessionStateEntry
  stop(sessionID: string): void
  rearm(sessionID: string): void
  reset(sessionID: string): void
  sweep(now: number): void
}

export const isTransientFailure = (error: { type: string; status?: number }): boolean =>
  TRANSIENT_ERROR.test(error.type) ||
  error.status === 408 ||
  error.status === 429 ||
  (error.status !== undefined && error.status >= 500)

const createAutoContinueState = (): AutoContinueState => {
  const sessions = new Map<string, SessionStateEntry>()
  const entry = (sessionID: string): SessionStateEntry => {
    const existing = sessions.get(sessionID)
    if (existing !== undefined) {
      existing.lastTouchedAt = Date.now()
      return existing
    }
    const fresh: SessionStateEntry = {
      handledAt: 0,
      consecutive: 0,
      lastTouchedAt: Date.now(),
      stopped: false,
    }
    sessions.set(sessionID, fresh)
    return fresh
  }
  return {
    sessions,
    touch: entry,
    stop(sessionID: string): void {
      const current = entry(sessionID)
      current.stopped = true
      current.consecutive = 0
    },
    rearm(sessionID: string): void {
      entry(sessionID).stopped = false
    },
    reset(sessionID: string): void {
      const current = entry(sessionID)
      current.stopped = false
      current.consecutive = 0
    },
    sweep(now: number): void {
      for (const [sessionID, current] of sessions) {
        if (now - current.lastTouchedAt > AUTO_CONTINUE_STATE_TTL_MS) {
          sessions.delete(sessionID)
        }
      }
    },
  }
}

export default Plugin.define({
  id: "sticky-retry",
  setup: async (ctx) => {
    const globals = globalThis as { [key: symbol]: unknown }
    let proxy = globals[PROXY_GUARD] as ProxyHandle | null | undefined
    if (proxy === undefined) {
      proxy = startProxy()
      globals[PROXY_GUARD] = proxy
    }

    const registrations: Array<{ dispose(): Promise<void> }> = []

    // The hook is registered in EVERY plugin instance (one per location) —
    // session events are only delivered to the matching location. The
    // rewrite itself is idempotent: an already-rewrapped baseURL (this or
    // another instance's) is skipped by the proxy-origin prefix check.
    if (proxy !== null) {
      const proxyOrigin = `http://127.0.0.1:${proxy.port}`
      registrations.push(
        await ctx.session.hook("model.request", (event) => {
          try {
            const base = event.baseURL
            if (base === undefined || base === "") return
            if (!/^https?:\/\//i.test(base)) return
            if (base.startsWith(`${proxyOrigin}/`)) return
            event.baseURL = `${proxyOrigin}${PATH_PREFIX}${encodeURIComponent(base)}`
            log(`rewrite ${event.model.providerID} -> ${originAndPath(base)}`)
          } catch {
            // never break the request over a rewrite failure
          }
        }),
      )
      log(`rewrite armed -> ${proxyOrigin}${PATH_PREFIX}<original>`)
    }

    registrations.push(
      await ctx.session.hook("retry", (event) => {
        try {
          const decision = decide({
            status: event.error.status,
            transportError: event.error.status === undefined,
            attempt: event.attempt,
          })
          if (decision.retry) {
            log(
              `fallback-retry attempt=${event.attempt} type=${event.error.type} delay=${Math.round(decision.delayMs / 1000)}s`,
            )
            event.decision = { retry: true, delay: decision.delayMs }
          }
        } catch {
          // never break the retry pipeline over the fallback
        }
      }),
    )

    // Auto-continue: subscribed in every instance because session events
    // are only delivered to the matching location. The global state map
    // dedupes when several instances see the same event. The handler never
    // awaits long: the delayed synthetic prompt is scheduled, so an
    // interrupt arriving during the wait is still honored in the callback.
    const state =
      (globals[AUTO_CONTINUE_GUARD] as AutoContinueState | undefined) ?? createAutoContinueState()
    globals[AUTO_CONTINUE_GUARD] = state
    const subscription = new AbortController()
    void (async () => {
      // The server event stream can break (observed: InterruptError on
      // /api/event when a session runner dies). Resubscribe forever while
      // this plugin instance is loaded, or auto-continue silently dies.
      while (!subscription.signal.aborted) {
        try {
          log("event-subscribe opening")
          for await (const event of ctx.event.subscribe({ signal: subscription.signal })) {
            try {
              scheduleAutoContinue(ctx, state, event, subscription.signal)
            } catch {
              // one bad event must never break the stream
            }
          }
          log("event-subscribe ended")
        } catch (error) {
          log(`event-subscribe error: ${scrubErrorText(error)}`)
        }
        if (subscription.signal.aborted) return
        await abortableSleep(5_000, subscription.signal).catch(() => {})
      }
    })()

    log(`setup complete (proxy=${proxy === null ? "inert" : `port ${proxy.port}`}, auto-continue=on)`)
    return () => {
      subscription.abort()
      for (const registration of registrations) void registration.dispose()
    }
  },
})

interface SessionFailureEvent {
  readonly type: string
  readonly data?: {
    readonly sessionID?: string
    readonly error?: { readonly type: string; readonly message?: string; readonly status?: number }
  }
}

/** Fast, never-blocking event routing. The only slow part — waiting out the
 *  retry delay before re-prompting — happens inside a timer callback that
 *  re-checks the user-interrupt flag right before sending. */
const scheduleAutoContinue = (
  ctx: Plugin.Context,
  state: AutoContinueState,
  event: SessionFailureEvent,
  subscriptionSignal: AbortSignal,
): void => {
  const sessionID = event.data?.sessionID
  if (sessionID === undefined) return
  state.sweep(Date.now())

  if (event.type === "session.execution.interrupted") {
    state.stop(sessionID)
    return
  }
  if (event.type === "session.execution.started") {
    // A new execution after an interrupt means the user re-engaged: re-arm
    // auto-continue. The consecutive-failure counter is NOT reset here —
    // our own synthetic continue also fires this event, and the schedule
    // must keep growing across outage cycles.
    state.rearm(sessionID)
    return
  }
  if (event.type === "session.execution.succeeded") {
    state.reset(sessionID)
    return
  }
  if (event.type !== "session.execution.failed") return
  const error = event.data?.error
  if (error === undefined) return
  if (!isTransientFailure(error)) {
    log(`auto-continue skip session=${sessionID} non-transient type=${error.type}`)
    return
  }
  const entry = state.touch(sessionID)
  if (entry.stopped) {
    log(`auto-continue skip session=${sessionID} user interrupted`)
    return
  }
  const now = Date.now()
  if (now - entry.handledAt < AUTO_CONTINUE_DEDUPE_MS) return
  if (entry.consecutive >= MAX_AUTO_CONTINUES) {
    log(`auto-continue cap reached session=${sessionID} (${MAX_AUTO_CONTINUES}) — giving up`)
    return
  }
  entry.handledAt = now
  entry.consecutive += 1
  const attempt = entry.consecutive
  const delayMs = scheduleDelayMs(attempt)
  log(
    `auto-continue session=${sessionID} attempt=${attempt} type=${error.type} in=${Math.round(delayMs / 1000)}s`,
  )
  setTimeout(() => {
    const fresh = state.sessions.get(sessionID)
    if (subscriptionSignal.aborted) return
    if (fresh === undefined || fresh.stopped) {
      log(`auto-continue cancelled session=${sessionID}`)
      return
    }
    void ctx.session
      .synthetic({
        sessionID,
        text: `[sticky-retry] provider outage (${error.type}) — auto-continuing, attempt ${attempt}.`,
      })
      .then(() => log(`auto-continue sent session=${sessionID} attempt=${attempt}`))
      .catch((error: unknown) =>
        log(`auto-continue failed session=${sessionID}: ${scrubErrorText(error)}`),
      )
  }, delayMs)
}
