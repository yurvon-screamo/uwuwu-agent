---
title: "Why I built a memory proxy with NLI contradiction detection (instead of cosine similarity)"
published: false
description: "Cosine similarity gave me avg 0.82 between contradicting fact pairs. Here is what I did about it, and the Rust architecture I ended up shipping."
tags: rust, ai, llm, showdev
---

# Why I built a memory proxy with NLI contradiction detection

> This is a draft. `published: false` until the user approves. After approval, flip to `published: true` via `PUT /articles/{id}` or the dev.to UI.
>
> Tags: `rust, ai, llm, showdev` (4 lowercase, no spaces — dev.to requirement).
>
> `canonical_url` is intentionally OMITTED: this is the PRIMARY publication, not a cross-post. Add `canonical_url` only if/when the article is republished elsewhere (own blog, Hashnode in Phase 4) and dev.to should defer SEO to the original. See `tool-integration-devto` skill → Cross-posting.

## The problem with cosine similarity for memory

Every long-running LLM agent I've shipped eventually started lying to itself. The conversation history grew, the memory layer pulled in candidates, and a few turns later the model was reasoning over two facts that directly contradicted each other.

The standard fix in the LLM-memory niche is cosine similarity over fact embeddings. I assumed it would work and wired it up. Then I measured it.

On my fixture corpus, the **average cosine similarity between contradicting fact pairs was 0.82**, and **4 of 16 pairs scored above 0.92**. Cosine sees "X is true" and "X is false" as nearly the same sentence. Contradiction and entailment live in the same neighborhood of embedding space.

That is when I stopped trusting cosine for anything other than candidate selection and delegated the verdict to a Natural Language Inference model.

## What NLI gives you that cosine cannot

A DeBERTa-v3 NLI model (trained on MultiNLI / FEVER / ANLI / Ling / WANLI) takes a premise and a hypothesis and returns three scores: **entailment**, **neutral**, **contradiction**. For memory finalize, the premise is an existing fact and the hypothesis is a new candidate. The verdict drives one of three outcomes:

- **Entailment** — the candidate is supported. Promote its confidence (multi-source bonus).
- **Neutral** — neither confirms nor denies. Leave it pending.
- **Contradiction** — the candidate conflicts with an existing fact. Reject the lower-confidence one, or surface the conflict.

Cosine answers "are these sentences textually similar?", which is the wrong question. NLI answers "does this new claim follow from, contradict, or stay independent of what I already know?", which is the right question.

## The architecture I ended up shipping

SMOS is an OpenAI-compatible semantic memory proxy written in Rust. Strict hexagonal / DDD three-crate workspace:

```
smos-rust/
├── smos-domain/         # pure domain (NO IO, NO async runtime)
├── smos-application/    # ports (traits), DTOs, use cases
└── smos-adapters/       # the ONLY crate where IO happens
```

The dependency direction is one-way — `domain ← application ← adapters` — and enforced by the workspace layout.

The application layer declares ports as `async fn` in trait. The trait is `Send`-bounded at the adapter call site, **not** at the port definition, so the application layer stays runtime-agnostic. You could swap tokio for async-std at the adapter level without touching a use case.

Storage is **embedded SurrealDB 2.x on RocksDB with HNSW**. There is no external database process to run. One binary, one directory on disk. The NLI verdict comes from a Python DeBERTa-v3 sidecar spawned as a subprocess; the first `smos serve` startup downloads the ~1.5 GB model into the Hugging Face cache.

## The pipeline

Every chat-completion request flows through five stages:

```
            ┌─────────────────┐
request ──▶ │ 1. parse model  │  strip `memory_key:` prefix
            │    + session id │  detect/reuse session from trailing markers
            └────────┬────────┘
                     ▼
            ┌─────────────────┐
            │ 2. enrich       │  embed topic → vector search → rerank → dedup
            │   (fail-open)   │  inject <smos-memory> block; never FAILS request
            └────────┬────────┘
                     ▼
            ┌─────────────────┐
            │ 3. upstream     │  reqwest POST → OpenAI-shaped upstream
            │   forward       │  SSE passthrough + session marker injection
            └────────┬────────┘
                     ▼
            response streamed back to client; THEN:
            ┌─────────────────┐
            │ 4. extract      │  spawn background task (ExtractionSupervisor)
            │   (post-resp.)  │  LLM pulls facts from assistant content
            └────────┬────────┘
                     ▼
            ┌─────────────────┐
            │ 5. finalize     │  SessionWatcher triggers on timeout/overflow:
            │   (background)  │  NLI contradiction/merge, confidence promotion
            └─────────────────┘  (DeBERTa-v3 Python sidecar)
```

The fail-open contract on stage 2 is the part I want to call out. Enrichment runs synchronously on the request path (it must complete before the upstream forward), but a failure there cannot block or fail the request. If the vector store is degraded or the reranker times out, the request still reaches the upstream — just without the `<smos-memory>` block. A memory layer that can fail the user's request is a deployment hazard.

## Quick start

```bash
# Build everything.
cargo build --workspace

# Start the proxy (default upstream: Ollama on http://localhost:11434).
cargo run --bin smos -- serve

# Send an OpenAI-shaped request.
curl http://127.0.0.1:8888/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
        "model": "origa:gpt-4o",
        "stream": true,
        "messages": [{"role":"user","content":"hello"}]
      }'
```

The `model` field may carry a memory-key prefix (`memory_key:real_model`); the prefix is stripped before the request reaches the upstream. A bare model name maps to the `shared` memory namespace.

## Test coverage

SMOS ships with a tiered test strategy so the feedback loop matches the task at hand:

| Tier | Command | Scope | Tests | Warm time |
|------|---------|-------|-------|-----------|
| Unit | `cargo tf` | `smos-domain` + `smos-application` unit tests only | 345 | ~0.6 s |
| Fast | `cargo t` | Full workspace unit tests + fast integration binaries | 533 | ~2.6 s |
| Integration | `cargo ti` | Adds every embedded-SurrealDB gate (superset of `cargo t`) | 643 | ~50 s |
| External | `cargo tall` | Adds Python DeBERTa sidecar + live Ollama | 643 + external | 10+ min |

All 8 slices are landed and production-ready.

## Known limitations (the honest part)

1. **CORS is permissive by default.** Ships as `CorsLayer::permissive()` for browser-driven OpenAI clients. Safe because the default bind is `127.0.0.1`; add an explicit `[server].allowed_origins` field before any non-localhost deploy.
2. **`find_session` is O(N) `snapshot_all`.** `FinalizeSession` loads every session row and filters in Rust. Fine for thousands of sessions; a typed SurrealQL `SELECT … WHERE id = $id` fast path is a future slice.
3. **Vector search cannot push equality filters into the HNSW traversal.** They are applied as a Rust-side post-filter. A brute-force cosine fallback kicks in whenever HNSW under-serves — a full table scan with cosine similarity in SurrealQL, slower than the index path.
4. **Cosine drift walk recall = 100%, merge recall = 50%** on the POC fixture corpus. The merge-recall gap is part of why NLI owns the verdict and cosine does not.
5. **Persona injection deferred.** The `memory_key/persona.md` block is not yet wired; the domain builder already accepts `memory_key` for forward compatibility.
6. **The session watcher's mid-cycle shutdown is bounded by the in-flight cycle.** A `shutdown_rx` signal that arrives mid-cycle is observed only after the cycle completes. The per-cycle latency scales with the pending backlog; `shutdown_extraction_grace_seconds` bounds the subsequent drain.

## What I'd do differently

If I were starting over, I would not have started with cosine at all. I spent two weeks tuning thresholds that did not generalize. The empirical signal ("0.82 average between contradicting pairs") was there in the first fixture run; I did not look at it carefully enough. NLI from day one would have saved the detour.

The other thing I underestimated: how much the fail-open contract matters for adoption. People do not deploy a memory layer that can fail their request. Once I made enrichment fail-open by construction, the proxy stopped being a risk and started being infrastructure.

## Source

GitHub: [yurvon-screamo/smos](https://github.com/yurvon-screamo/smos)

Happy to answer questions in the comments — especially on the NLI-vs-cosine trade-off and on the embedded-SurrealDB decision.
