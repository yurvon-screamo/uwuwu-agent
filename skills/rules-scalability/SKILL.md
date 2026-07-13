---
name: rules-scalability
description: Stack-neutral scalability rules for HTTP APIs and services. Covers three related concerns — capacity planning, load testing, and scale-driven architecture. SLOs (p95/p99, error rate), load-test loops, finding the first bottleneck, headroom, load shedding, and patterns for moving work off the request path (cache, pool tuning, queue/outbox, backpressure). Use when load testing, planning capacity/scaling, designing for scale, choosing sync vs async/queue, setting latency/error SLOs, or answering "can it handle the load?".
---

# Scalability

Scalability is how a system behaves as load grows. These rules apply in three moments where that matters — they share the same metrics and the same loop:

- **Capacity planning** — how much traffic can it take before UX degrades?
- **Load testing** — how do you measure that, for real, instead of guessing?
- **Architecture for scale** — how do you shape the system so the answer gets better?

The output is always a NUMBER — measured, with the conditions it was measured under written next to it. Not a feeling, not a vibe about instance sizes.

## The real question

Capacity planning answers ONE question:

> How much traffic can this API take before the user experience starts to degrade?

NOT "do we have spare CPU/RAM." Everything else is in service of this.

The capacity limit is the point where the API stops being fast — not the point where it errors out.

## Why CPU and RAM lie

The things that break first rarely show up as CPU pressure. The API falls over while CPU sits at 40%.

- **Connection pool exhaustion** — out of DB/HTTP connections long before the DB runs out of CPU; requests queue waiting for a free one.
- **Thread/fiber pool starvation** — a few blocking calls under load; latency explodes while no single resource looks maxed.
- **Lock contention** — a hot lock turns parallel work into a single-file line.
- **Queue lag** — background processing falls behind; the write "succeeds" but the user can't see the result for 30s.
- **p95 latency collapse** — the average still looks great while one request in twenty times out.

When designing architecture, assume these — not CPU — are the real ceilings, and design so they're observable and tunable.

## Measure these, not averages

Averages hide the people who are actually suffering. Average 80ms + p99 4s = a real slice of users having a miserable time while your dashboard lies about it.

Roughly in order of how often they catch the real problem:

1. **p95 latency** — the tail is where pain lives. Day-to-day SLO. (If you add only one metric, make it p95 per endpoint.)
2. **p99 latency** — how bad the bad moments get.
3. **RPS per endpoint class** — not one global number. A read endpoint and a write endpoint have nothing in common.
4. **Concurrent / in-flight requests** — what actually pressures pools and threads.
5. **Timeout rate and error rate** — the difference between "slow" and "broken."
6. **DB/dependency connection pool usage** — the most common silent ceiling.
7. **Queue depth and processing lag** — for anything async, how far behind the workers are.

This list is the same whether you're load testing, planning capacity, or deciding what an architecture must make observable.

## The load testing loop

The same loop drives capacity planning. It is not a document you write once — order matters.

1. **Define the SLO first** — "p95 < 300ms, error rate < 1%" is testable; "fast" is not. Pin the number BEFORE you run anything, or you'll move the goalposts to wherever the results land.
2. **Steady-state test** — a load you believe reflects normal traffic. This is the reference point (cruising altitude).
3. **Spike test** — ramps hard and fast. Steady-state tells you cruising altitude; the spike tells you what happens when marketing sends an email at noon.
4. **Find the FIRST bottleneck and fix ONLY that** — when the system buckles, something gives way first. Fix it, then re-test, because the fix usually just moves the ceiling to the next bottleneck. You want to watch it move.
5. **Add headroom, then publish** — once inside SLO, leave 30–50% margin over expected peak and WRITE DOWN the number with its conditions. A capacity limit nobody can find is the same as no capacity limit.

Re-run on every meaningful architecture or DB change, and at minimum once per release cycle. Three months of features on top of an old number = the number no longer holds.

## Scaling patterns by load

Numbers depend on hardware, queries, and SLO. When there's nothing else to go on — this is also the first architectural move at each tier:

| Load | First move (code or architecture) |
|---|---|
| < 100 RPS | Don't reach for infra. Fix N+1 queries and allocations. At this level APIs are slow because of code, not capacity. |
| 100–1000 RPS | Caching + connection pool tuning. Right-size the pool, put a cache in front of hot reads. |
| 1000+ RPS on writes | Stop writing synchronously to the DB on the request path. Queue-based load leveling: accept fast, process behind a worker (outbox pattern). |
| High burst + strict latency SLO | Explicit rate limiting + backpressure. Decide the shed point on purpose, ahead of time. |

The pattern across all four: as load grows, work moves OFF the request path. Reads get cached, writes get queued, the synchronous critical section gets as small as you can make it. When designing a system, ask of every operation: does this have to be synchronous on the request path, or can it move off it?

## Load shedding is a feature, not a bug

Past a concurrency limit, prefer a 429/503 over falling over and serving nobody.

- Shed deliberately and fast — a short bounded wait (a few hundred ms), then reject instead of queueing forever.
- A 429 under extreme load is a GOOD outcome — the system protected the requests it can serve instead of accepting everything and degrading all of it.
- Decide the shed point on purpose, ahead of time, rather than discovering it in production.

This applies at design time too: a system with a defined shed point degrades predictably; a system without one fails catastrophically.

## Watch during a spike

- **p95 latency** — is the accepted path staying fast?
- **Reject (429/503) rate** — load shedding kicking in, exactly as designed.
- **Queue depth / pending async work** — if it climbs and never drains, the workers can't keep up with the write rate. That's a capacity limit too, just on the async side.

## The goal

Never zero errors. The goal is **predictable degradation** — knowing precisely what the API does when you push past its limit, so the failure is boring instead of catastrophic.

## Checklist before calling it "capacity planned" or "designed for scale"

- [ ] Every endpoint class has a written SLO (p95 target + error budget).
- [ ] Load test scripts live in source control next to the code, run on every meaningful change.
- [ ] There is a known, documented max safe RPS per endpoint class.
- [ ] A runbook exists for scaling up AND down — scaling down is where the surprises hide.
- [ ] Alerts fire on p95, timeout rate, and queue lag — NOT just CPU and memory.

If even one is missing, you're back to guessing on the next release call.
