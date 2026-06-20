# Reddit — r/LocalLLaMA launch (paste-ready)

> PASTE-READY COPY. Do not auto-post (Reddit bans API posting + LLM-detection).
> The user pastes the title + body into r/LocalLLaMA's submit form manually.
>
> 10:1 rule: confirm that account `turbin_y` already has ≥10 useful contributions in r/LocalLLaMA before this self-promo post. If not, defer until the karma ratio is satisfied.

## Subreddit

```
r/LocalLLaMA
```

## Title

```
I built an OpenAI-compatible memory proxy in Rust with NLI contradiction detection
```

(Self-identified expertise in the title — "I built", not "Introducing X". No emoji.)

## Flair / tag

If r/LocalLLaMA supports flair, use **Show & Tell** (or the subreddit's equivalent showcase flair).

## Body

```
I've been building local-LLM infrastructure for a while, and every long-running
agent I shipped eventually started lying to itself: the model reasoned over
contradicting facts because the memory layer never verified them. So I built
SMOS — an OpenAI-compatible semantic memory proxy in Rust.

It sits between your OpenAI-shaped client and your upstream (Ollama, OpenAI,
OpenRouter, …). Before the request reaches the model it pulls in relevant facts;
after the response it extracts new ones; in the background it runs an
NLI finalize pass that promotes, merges, or rejects candidates.

**Why NLI and not cosine.**

On my fixture corpus, the average cosine similarity between contradicting fact
pairs was 0.82 — 4 of 16 pairs scored above 0.92. Cosine sees "X is true" and
"X is false" as nearly the same sentence. So I delegated the verdict to a
Python DeBERTa-v3 sidecar. Cosine only feeds candidate selection; NLI owns the
contradiction/merge decision.

**Pipeline.**

    request → parse model + session
           → enrich (fail-open — never blocks the request)
           → upstream forward (SSE passthrough + session-marker injection)
           → response streamed back to client
           → THEN background: extract facts (LLM)
           → THEN background: finalize (DeBERTa NLI contradiction/merge)

**What it looks like in code.**

Hexagonal / DDD three-crate workspace in Rust (edition 2024, MSRV 1.96):

    smos-rust/
    ├── smos-domain/         # pure domain (NO IO, NO async runtime)
    ├── smos-application/    # ports (traits), DTOs, use cases
    └── smos-adapters/       # the ONLY crate where IO happens

Storage is embedded SurrealDB 2.x on RocksDB with HNSW. There is no external
database process to run — one binary, one directory on disk.

Quick start:

    cargo run --bin smos -- serve
    curl http://127.0.0.1:8888/v1/chat/completions \
      -H 'content-type: application/json' \
      -d '{"model":"origa:gpt-4o","stream":true,"messages":[{"role":"user","content":"hello"}]}'

**Results.**

| Tier        | Command   | Tests | Warm time |
|-------------|-----------|-------|-----------|
| Unit        | cargo tf  | 345   | ~0.6 s    |
| Fast        | cargo t   | 533   | ~2.6 s    |
| Integration | cargo ti  | 643   | ~50 s     |

All 8 slices are landed and production-ready.

**Known limitations.**

- CORS is permissive by default (safe on 127.0.0.1; add `[server].allowed_origins`
  before any non-localhost deploy).
- Vector search cannot push equality filters into HNSW; they are applied as a
  Rust-side post-filter.
- Persona injection (`memory_key/persona.md`) is deferred — the domain builder
  already accepts `memory_key` for forward compatibility.
- The finalize watcher's mid-cycle shutdown is bounded by the in-flight cycle
  (the `shutdown_rx` signal is observed only after the current cycle completes).

**Open questions I'd appreciate feedback on.**

- Has anyone shipped a memory layer that handles fact contradiction explicitly?
  Most of the ones I looked at lean on cosine and call it a day.
- Is the DeBERTa-v3 sidecar (Python subprocess, ~1.5 GB model) acceptable as a
  deploy dependency, or would you prefer a pure-Rust NLI path even if it means
  a smaller model?
- For r/LocalLLaMA specifically — what is the cleanest Ollama setup you'd want
  to see in a quickstart?

Source: https://github.com/yurvon-screamo/smos

Happy to answer anything in the comments.
```

## Pre-post checklist

- [ ] Account `turbin_y` has ≥10 useful contributions in r/LocalLLaMA (10:1 rule).
- [ ] Flair set to **Show & Tell** if the subreddit supports it.
- [ ] Body starts with "I've been building..." (self-identified expertise).
- [ ] At least one code snippet is included (Reddit anti-LLM sentiment; visible hand-written structure).
- [ ] Open questions section is present (closes the loop with the community).
- [ ] .factcheck.json `gate: READY` confirmed.
