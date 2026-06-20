# Show HN — smos-rust launch (paste-ready)

> PASTE-READY COPY. Do not auto-submit. The user pastes the **Title** into the HN submit field, then pastes the **First comment** immediately after the post appears.
>
> Submit window: **Tue / Wed / Thu, 7–9 AM EST**. Author must be at the keyboard for the first 2 hours to answer every comment.

## Title

```
Show HN: SMOS – OpenAI-compatible semantic memory proxy (Rust)
```

(62 characters — well under the 80-char HN truncation limit. Plain description, en-dash, no marketing words, no emoji.)

## URL

```
https://github.com/yurvon-screamo/smos
```

(Link to the repo, NOT a landing page. HN downvotes landing pages.)

## First comment (post within seconds of submission, 300–500 words)

```
The problem.

I've been building local-LLM infrastructure for a while, and every long-running
agent I shipped eventually started lying to itself. Conversation history grew,
fact extraction pulled in candidates, and a few turns later the model was
reasoning over two facts that directly contradicted each other. Cosine similarity
did not help: on my fixture corpus, the average cosine similarity between
contradicting fact pairs was 0.82, and 4 of 16 pairs scored above 0.92. Cosine
thinks "X is true" and "X is false" are nearly the same sentence.

What I built.

SMOS is an OpenAI-compatible semantic memory proxy written in Rust. It sits
between an OpenAI-shaped client and an OpenAI-compatible upstream (Ollama,
OpenAI, OpenRouter, …). Before the request reaches the LLM it retrieves relevant
facts; after the response it extracts new facts; in the background it runs an
NLI finalize pass that promotes, merges, or rejects candidates.

Architecture.

Strict hexagonal / DDD three-crate workspace. smos-domain (pure, no IO) ←
smos-application (ports + use cases, async-fn-in-trait, runtime-agnostic) ←
smos-adapters (the only crate that does IO). The application layer is not
pinned to tokio — port traits are Send-bounded at the adapter call site, not
at the port definition.

Storage is embedded SurrealDB 2.x on RocksDB with HNSW. There is no external
database process to run. The NLI verdict comes from a Python DeBERTa-v3 sidecar;
cosine only feeds candidate selection, never the contradiction/merge decision.

Pipeline: parse model + session → enrich (fail-open, never blocks the request)
→ upstream forward with SSE + session-marker injection → post-response
extraction (background) → finalize on timeout/overflow (background).

Benchmarks.

| Tier        | Command   | Tests | Warm time |
|-------------|-----------|-------|-----------|
| Unit        | cargo tf  | 345   | ~0.6 s    |
| Fast        | cargo t   | 533   | ~2.6 s    |
| Integration | cargo ti  | 643   | ~50 s     |

Known limitations.

- CORS is permissive by default. Safe on 127.0.0.1; add [server].allowed_origins
  before any non-localhost deploy.
- find_session is O(N) snapshot_all. Fine for thousands of sessions; a typed
  SurrealQL fast path is a future slice.
- Vector search cannot push equality filters into the HNSW traversal; they are
  applied as a Rust-side post-filter.
- Persona injection (the memory_key/persona.md block) is deferred — the domain
  builder already accepts memory_key for forward compatibility.

Source: https://github.com/yurvon-screamo/smos

Happy to answer questions.
```

## Pre-submit checklist

- [ ] Submitted Tue / Wed / Thu between **7–9 AM EST**.
- [ ] Title is 80 chars or fewer (HN truncates longer).
- [ ] URL points to the GitHub repo (not a landing page).
- [ ] First comment is ready to paste within 30 seconds of submission.
- [ ] Calendar cleared for the first 2 hours — answer every comment.
- [ ] .factcheck.json `gate: READY` confirmed before submitting.

## Post-submit checklist

- [ ] Record the HN `objectID` (from Algolia or the URL) immediately.
- [ ] Poll points/comments every 1h for the first 6h via `tool-integration-hn`.
- [ ] Snapshot at T+24h, T+48h, T+72h into `metrics/smos-rust/YYYY-Www.md`.
