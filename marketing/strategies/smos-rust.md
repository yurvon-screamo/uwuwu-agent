# Marketing strategy — smos-rust

> Pilot product for the @marketer agent. This strategy is the **single source of truth** for smos-rust messaging, channels, and launch sequencing. All factual claims cite the README line numbers; re-verify before each publication.

## Product overview

**smos-rust** is the Rust port of SMOS — an OpenAI-compatible **semantic memory proxy**. It sits between an OpenAI-shaped client and an OpenAI-compatible upstream (Ollama, OpenAI, OpenRouter, …) and adds long-term memory to every conversation: it retrieves relevant facts before the request reaches the LLM, extracts new facts from the response, and runs an NLI-driven finalize pass that promotes / merges / rejects candidates (README:3-7).

Stack: Rust (edition 2024, MSRV 1.96), strict hexagonal / DDD three-crate workspace (`smos-domain` ← `smos-application` ← `smos-adapters`), embedded SurrealDB 2.x (RocksDB + HNSW), Python DeBERTa-v3 sidecar for NLI, axum 0.8 HTTP server, reqwest 0.12 upstream, tokio 1 runtime. License: MIT (README:428, Cargo.toml:9).

Status: **Production-ready — all 8 slices landed** (README:16). Test surface: 533 fast (`cargo t`), 643 integration (`cargo ti` — superset), 345 domain/application unit (`cargo tf`) (README:29-31, README:248-253).

Target audience: AI/dev tooling engineers, LLM application builders, Rust community, local-LLM community.

## Differentiation Framework

> The differentiation table is the **first mandatory artifact** — without it, every message is a generic "memory proxy" claim.

### Competitor landscape

> ⚠️ **All competitor metrics below are research snapshots from 2026-06-19** (sourced from marketing research notes via `tvly search` / `gh api` / `crates.io`). They are NOT in `.factcheck.json` because the public launch drafts make NO specific competitor claims. **If a competitor-comparison post is ever requested, every number below MUST be live-reverified** (`tvly search site:github.com/<repo>`, `gh api repos/<repo>`, `curl crates.io/api/v1/crates/<name>`) and added to a new `.factcheck.json` cycle before publication.

| Project | Stack | License | Key differentiator vs smos-rust | Snapshot sources (2026-06-19) |
|---------|-------|---------|--------------------------------|-------------------------------|
| **Mnemo** (watzon/mnemo) | Rust | MIT | Near-identical scope (transparent HTTP proxy for LLM long-term memory). ~5 GitHub stars, ~70 crates.io downloads. smos-rust leads on test coverage (533+643) and on NLI contradiction detection. | `github.com/watzon/mnemo`, `crates.io/crates/mnemo` |
| **Memzent.AI** | Go + Rust | Apache 2.0 | Semantic proxy, multi-language. ~2 GitHub stars. No NLI contradiction verdict; no embedded SurrealDB. | `github.com/...memzent` (reverify exact path before publish) |
| **Reflex** (rawcontext/reflex) | Rust | AGPL-3.0 | Episodic memory + semantic cache; ~40% token-savings claim (competitor self-claim, not independently verified). ~0 GitHub stars. AGPL-3.0 limits adoption (copyleft on derivative services). | `github.com/rawcontext/reflex` |
| **linggen-memory** | Rust | MIT | LanceDB + MCP. ~106 GitHub stars — widest current adoption in the niche. No embedded SurrealDB; no NLI verdict; depends on LanceDB. | `github.com/...linggen-memory` (reverify exact path before publish) |
| **mememory** (scott-walker) | Go | MIT | MCP server + PostgreSQL + pgvector. Requires an external PostgreSQL + pgvector deployment — higher operational cost than smos-rust's single-binary + directory. | `github.com/scott-walker/mememory` |

### smos-rust real differentiators (from README)

| Differentiator | README citation | Why it matters |
|----------------|-----------------|----------------|
| **NLI contradiction-detection (DeBERTa verdict, not cosine)** | README:147 (`[merge]` config: "cosine candidate-selection threshold for merge detection"), README:379-382 (limitation §1: "Cosine embeddings are not used for contradiction / merge detection… avg similarity 0.82 between contradicting pairs, 4/16 above 0.92. Cosine only feeds candidate selection; DeBERTa NLI owns every verdict.") | Cosine similarity cannot reliably separate contradicting claims — empirical fact measured on the POC fixture corpus. DeBERTa NLI gives a verdict per candidate pair. This is the **primary** technical differentiator vs every competitor listed above. |
| **Fail-open enrichment** | README:101-102 (pipeline §2: "its fail-open contract guarantees no enrichment failure can block or fail the request"), README:112 ("inject `<smos-memory>` block; never FAILS request") | A memory layer that can fail the user's request is a deployment hazard. smos-rust's enrichment failure degrades gracefully — the request always reaches the upstream. |
| **Session-marker injection** | README:117 (pipeline §3: "SSE passthrough + session marker injection"), README:418 ("injects a session marker into the response so the next request in the same conversation can be linked") | Links conversation turns across stateless OpenAI-shaped clients without requiring the client to track session state. |
| **Embedded SurrealDB RocksDB (no external DB process)** | README:76 (architecture tree: `src/storage/ # SurrealStore (embedded RocksDB)`), README:412-414 ("SurrealDB — embedded RocksDB. Set `[surreal].path` to a persistent directory on disk… No external database process is needed.") | Single-binary deploy. Competitors (mememory with PostgreSQL, linggen with LanceDB) require separate DB provisioning. |
| **Runtime-agnostic async ports** | README:89-91 ("`smos-application` declares ports as `async fn` in trait — the trait is `Send`-bounded at the adapter call site, not at the port definition, so the application layer stays runtime-agnostic.") | Application layer is not pinned to tokio. Adapter swaps (e.g., a `smos-runtime-async-std`) are possible without touching use cases. |
| **Test coverage** | README:29-31, README:248-253 | 533 fast / 643 integration / 345 unit. Tiered strategy with documented warm/cold timings. Most competitors in the niche ship with sparse tests. |
| **8/8 slices production-ready** | README:16 ("Production-ready — all 8 slices landed."), README:18-27 (slice table) | The whole pipeline (domain → storage → HTTP → enrichment → extraction → finalize+NLI → session watcher → import CLI) is implemented, not a partial prototype. |

## Target audience

| Segment | Where they live | Why they care |
|---------|-----------------|---------------|
| **AI/dev tooling engineers** | HN, GitHub, dev.to, r/LocalLLaMA | Build LLM apps, need memory that does not lie to the model |
| **LLM app builders** | r/LocalLLaMA, Discord (Latent Space, MLOps Community), dev.to | Need drop-in OpenAI-compatible proxy without DB ops overhead |
| **Rust community** | r/rust, crates.io, GitHub Rust Trending | Architectural reference (hexagonal/DDD, async-fn-in-trait, runtime-agnostic ports) |
| **local-LLM community** | r/LocalLLaMA, r/Ollama | Self-hostable, single-binary, no cloud dependency |

## Canonical messaging

3–5 key theses. Every piece of content must carry at least one.

1. **"A memory proxy that doesn't lie to your LLM."** — NLI contradiction detection (DeBERTa verdict, not cosine). Cosine averaged 0.82 between contradicting pairs in our fixture corpus.
2. **"Fail-open by design — never blocks your request."** — Enrichment failures degrade gracefully; the request always reaches the upstream.
3. **"Production-ready: 8/8 slices, 533+643 tests, embedded DB."** — Single-binary deploy, no external database process.
4. **"Runtime-agnostic hexagonal architecture in Rust."** — async-fn-in-trait at the port, Send-bounded at the adapter call site.
5. **"Session marker injection keeps conversation turns linked across stateless clients."**

## Staged launch plan (smos-rust)

> Cognee lesson (Show HN without momentum = 6 points, Feb 2025): multi-channel burst only works AFTER pre-launch traction.

| Stage | Action | Channel | HUMAN GATE | Tool |
|-------|--------|---------|------------|------|
| **0. Pre** | Finalize README badges, GitHub Topics (`ai, memory, llm, semantic-memory, openai-compatible, proxy, rust, self-hosted, agents, local-llm`), crates.io metadata | GitHub repo settings | PR → user merge | `tool-integration-github` |
| **1. Crates** | Publish `smos-domain`, `smos-application`, `smos-adapters` to crates.io | crates.io | User runs `cargo publish` | manual (user) |
| **2. Article** | Longform: "Architecture of a Rust memory proxy with NLI contradiction detection" | dev.to (draft) | User approves + publishes | `tool-integration-devto` |
| **3. Reddit organic** | r/LocalLLaMA Show&Tell + r/rust (10:1 rule satisfied first) | Reddit | User pastes manually | `tool-integration-reddit` |
| ~~4. Discord~~ | ~~Latent Space / MLOps / Ollama #showcase mentions~~ | Discord | **Human activity, OUT of @marketer scope** | — |
| **5. Show HN** | Submit Tue/Wed/Thu 7–9 AM EST with first comment ready | news.ycombinator.com | User submits manually | `tool-integration-hn` (monitoring only) |
| **6. Burst** | Within 48h of Show HN: GitHub release v1.0.0 + Product Hunt launch + cross-post dev.to + Reddit | GitHub release (PR → user), PH (browser paste) | User submits each manually | `tool-integration-github`, `tool-integration-producthunt`, `tool-integration-devto` |

**Recommended human activity** (NOT automated by @marketer — flagged for the user):
- Discord presence in Latent Space (`discord.gg/xJJMRaWCRt`), MLOps Community, Ollama — genuine participation before any showcase mention.
- Awesome-list PRs: `awesome-rust` requires 50+ stars OR 2000+ crates.io downloads — wait until threshold, then PR with alphabetical sort, template `[ACCOUNT/REPO](url) [[CRATE](url)] - DESCRIPTION`.

## Channel matrix

| Channel | Tone | Format | Timing | Success criteria (24–72h) |
|---------|------|--------|--------|---------------------------|
| **GitHub** | Technical docs first | README, badges, Topics, Release Notes, Discussions | Continuous | Stars ≥ 50, forks ≥ 5, traffic/views trending up |
| **Hacker News (Show HN)** | Factual, humble, technical-first | Title ≤ 80 chars + 300–500-word first comment | Tue/Wed/Thu 7–9 AM EST | Points ≥ 30, comments ≥ 20, reaches front page position ≤ 30 |
| **Reddit r/LocalLLaMA** | Helpful, self-identified expertise | Longform with code snippet, open question at end | Any weekday | Upvotes ≥ 50, comments ≥ 15 |
| **Reddit r/rust** | Substantive, no fluff | Architecture-focused post with trade-offs | Any weekday | Upvotes ≥ 30, comments ≥ 10 |
| **dev.to** | Longform tutorial / architecture deep-dive | Frontmatter + body_markdown, 4 lowercase tags, `published: false` first | 1–2 days before Show HN | Page views ≥ 500, reactions ≥ 25, comments ≥ 5 |
| **Product Hunt** | Maker comment = intro→problem→gap→solution→benefits→proof→offer | Gallery 635×380, tagline ≤ 60 chars | Tue–Thu 12:01 AM PST (after HN momentum) | Votes ≥ 100, comments ≥ 20, daily rank ≤ 15 |
| **X / Twitter** | Punchy, one-insight-per-tweet | Read-only monitoring + paste-ready threads | Manual publication ($100/mo Basic too expensive for MVP) | Impressions ≥ 5K, profile clicks ≥ 50 |

## Open risks

- **mnemo convergence.** Mnemo (Rust, MIT) has near-identical scope. The differentiation must lean hard on NLI verdict + embedded SurrealDB + test coverage. Never claim "first" or "only" without verification.
- **HN audience saturation with "memory" posts.** Mem0 / Letta / Cognee / Zep all launched on HN in 2023–2025. The pitch must distinguish smos-rust (OpenAI-compatible proxy, Rust, NLI) from app-level memory SDKs (Mem0/Letta Python SDKs).
- **PRAW / Reddit anti-LLM sentiment.** r/LocalLLaMA and r/rust are wary of AI-generated posts. First Reddit post must be visibly hand-written, with a concrete code snippet and an honest limitation.
- **Hashnode absence.** Cross-post pipeline currently dev.to-only. If Hashnode Pro is acquired later, add `tool-integration-hashnode` skill and cross-post with `canonical_url` set to dev.to.
