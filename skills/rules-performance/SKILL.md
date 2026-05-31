---
name: rules-performance
description: Frontend and backend performance rules. Use when optimizing page loading, rendering, API, database queries, caching, as well as when analyzing Core Web Vitals (LCP, INP, CLS) and backend metrics (latency, throughput).
---

# Performance

## Optimization Process

```
1. MEASURE → Baseline with real data
2. FIND    → Real bottleneck (not assumed)
3. FIX     → Eliminate the specific bottleneck
4. VERIFY  → Measure again, confirm improvement
5. PROTECT → Tests/monitoring against regression
```

Optimization without measurement is guessing. Profile first.

### Where to look for the problem

```
What's slow?
├── Initial load
│   ├── Large bundle? → Bundle size, code splitting
│   ├── Slow server? → TTFB in Network waterfall
│   │   ├── DNS slow? → dns-prefetch / preconnect
│   │   ├── TCP/TLS slow? → HTTP/2, keep-alive
│   │   └── Waiting (server) slow? → Profile backend, queries, cache
│   └── Render-blocking resources? → Network waterfall CSS/JS
├── Interaction lagging
│   ├── UI freezing? → Profile main thread, long tasks (>50ms)
│   ├── Input lagging? → Check re-renders, controlled components
│   └── Animation stuttering? → Layout thrashing, forced reflows
├── Navigation between pages
│   ├── Data loading? → API response time, waterfalls
│   └── Client-side rendering? → Profile components, N+1 fetch
└── Backend / API
    ├── One endpoint slow? → DB queries, indexes
    ├── All endpoints slow? → Connection pool, memory, CPU
    └── Periodic slowdowns? → Lock contention, GC pauses, external dependencies
```

## Core Web Vitals

| Metric | Good | Needs Improvement | Poor |
|--------|------|-------------------|------|
| LCP | ≤ 2.5s | ≤ 4.0s | > 4.0s |
| INP | ≤ 200ms | ≤ 500ms | > 500ms |
| CLS | ≤ 0.1 | ≤ 0.25 | > 0.25 |

## Frontend

### Images

- WebP/AVIF formats, `srcset` + `sizes` for responsive sizes
- LCP images: `fetchpriority="high"`, no lazy loading
- Below the fold: `loading="lazy"` + `decoding="async"`
- Explicit `width` and `height` (prevents CLS)

### JavaScript

- Bundle ≤ 200KB gzipped (initial load)
- Code splitting via `import()` for routes and heavy features
- Split long tasks (> 50ms) — the main lever for INP
- `scheduler.yield()` / `yieldToMain` in long loops
- `requestIdleCallback` for deferrable work (analytics, prefetch)
- Heavy computations — in Web Workers

### Fonts

- 2–3 families, 2–3 weights each
- WOFF2 only
- Self-hosted when possible (font CDN = DNS + TCP + TLS)
- LCP-critical fonts: `<link rel="preload" as="font" type="font/woff2" crossorigin>`
- `font-display: swap` (or `optional` for non-critical)
- Subset via `unicode-range`
- Variable fonts when many weights are needed (one file instead of many)
- `size-adjust`, `ascent-override`, `descent-override` to reduce CLS on swap

### CSS

- Critical CSS inline or preload
- No render-blocking CSS for non-critical styles
- No CSS-in-JS runtime in production (extraction only)

### Rendering

- Animations only via `transform` and `opacity` (GPU)
- Long lists — virtualization (`react-window`)
- `content-visibility: auto` + `contain-intrinsic-size` for hidden sections
- No `unload` and `Cache-Control: no-store` on HTML — preserves bfcache

### Network

- Static assets: long `max-age` + content hashing
- HTTP/2 or HTTP/3
- `preconnect` for known origins
- `fetchpriority` on critical resources (not just `<img>`)

## Backend

### API

- API response time < 200ms (p95)
- Pagination on ALL list endpoints — no queries without LIMIT
- N+1 — the most common anti-pattern:

```typescript
// BAD: N+1 — one query per task for the owner
const tasks = await db.tasks.findMany();
for (const task of tasks) {
  task.owner = await db.users.findUnique({ where: { id: task.ownerId } });
}

// GOOD: One query with join/include
const tasks = await db.tasks.findMany({
  include: { owner: true },
});
```

- Unbounded fetching — always limit:

```typescript
// BAD: All records
const allTasks = await db.tasks.findMany();

// GOOD: Pagination
const tasks = await db.tasks.findMany({
  take: 20,
  skip: (page - 1) * 20,
  orderBy: { createdAt: 'desc' },
});
```

### Database

- Indexes for all filterable and sortable columns
- Connection pooling to DB (don't open a connection per request)
- Check query plans (EXPLAIN / EXPLAIN ANALYZE) for slow queries
- Read replicas for heavy read workloads
- Batch operations instead of a loop of single inserts/updates

### Caching

```typescript
// In-memory cache for frequently read, rarely changed data
const CACHE_TTL = 5 * 60 * 1000; // 5 minutes
let cachedConfig: AppConfig | null = null;
let cacheExpiry = 0;

async function getAppConfig(): Promise<AppConfig> {
  if (cachedConfig && Date.now() < cacheExpiry) {
    return cachedConfig;
  }
  cachedConfig = await db.config.findFirst();
  cacheExpiry = Date.now() + CACHE_TTL;
  return cachedConfig;
}
```

- HTTP caching for static assets:

```typescript
app.use('/static', express.static('public', {
  maxAge: '1y',
  immutable: true, // Content hashing in file names
}));

// API responses
res.set('Cache-Control', 'public, max-age=300'); // 5 minutes
```

- CDN for static assets and geographically distributed users
- Redis for distributed cache (in-memory doesn't work with multiple instances)

### Memory and CPU

- Memory leaks: heap snapshot analysis, RSS/heap growth monitoring
- CPU spikes: profiling, check for regex backtracking, synchronous heavy computations
- Large API payloads → pagination, projections (select only needed fields)
- Streaming for large responses instead of buffering entirely

## Performance Budget

```
JavaScript bundle:   < 200KB gzipped (initial load)
CSS:                 < 50KB gzipped
Images:              < 200KB per image (above the fold)
Fonts:               < 100KB total
API response time:   < 200ms (p95)
Time to Interactive:  < 3.5s on 4G
Lighthouse Score:    ≥ 90
```

## Anti-patterns

| Anti-pattern | Fix |
|---|---|
| N+1 queries | Joins, includes, batch loading |
| Queries without LIMIT | Always paginate |
| No indexes | Indexes for filtered/sortable columns |
| Layout thrashing | Batch DOM reads, then batch writes |
| Unoptimized images | WebP, responsive sizes, lazy load |
| Main thread blocking | `scheduler.yield()`, Web Workers |
| Memory leaks | Cleanup listeners, intervals, refs |
| No caching | In-memory / Redis / CDN where appropriate |
| Large API payloads | Projections, pagination, streaming |
| Regex backtracking | Use atomic groups, limit backtracking |
