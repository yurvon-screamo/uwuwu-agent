# GitHub Release — v1.0.0 (paste-ready)

> PASTE-READY COPY. This is a **draft release notes**. The user creates the release via `gh release create v1.0.0 --notes-file ...` or the GitHub UI after merging the prep PR.
>
> HUMAN GATE: this is the **second gate** — first the PR (prep README badges, Topics, Release Notes) is approved and merged, then the user creates the release tag.

## Tag

```
v1.0.0
```

> Note: the workspace `Cargo.toml` declares intra-workspace crate versions `smos-domain = "0.1.0"` and `smos-application = "0.1.0"` (path dependencies for future independent publishes). The application binary version is proposed as `v1.0.0` to mark the first production-ready release ("all 8 slices landed", README:16). Confirm the tag matches the actual published crate version before running `cargo publish`.

## Release title

```
v1.0.0 — first production-ready release (all 8 slices landed)
```

## Release notes

```markdown
First production-ready release. All 8 slices have landed.

## Highlights

- **Full memory pipeline.** Five stages: parse → enrich (fail-open) → upstream forward (SSE + session-marker injection) → post-response extraction → background finalize with NLI contradiction/merge.
- **NLI contradiction detection (DeBERTa-v3).** Cosine similarity is used only for candidate selection. On the POC fixture corpus the average cosine similarity between contradicting fact pairs was 0.82 (4 of 16 pairs above 0.92), so cosine is not trusted for the verdict. DeBERTa NLI owns every contradiction/merge decision.
- **Embedded SurrealDB 2.x (RocksDB + HNSW).** No external database process to run. One binary, one directory on disk.
- **Runtime-agnostic hexagonal architecture.** Port traits are `async fn` without `Send`-bound; the trait is `Send`-bounded at the adapter call site, not at the port definition. The application layer is not pinned to tokio.
- **Fail-open enrichment.** Enrichment runs on the request path but cannot block or fail the request. A degraded vector store or a reranker timeout still reaches the upstream — without the `<smos-memory>` block, but with the response intact.
- **Session-marker injection.** SSE passthrough injects a session marker into the response so the next request in the same conversation can be linked across stateless OpenAI-shaped clients.
- **Unified `smos` binary.** Four subcommands: `serve` (proxy + watcher + sidecar), `import` (re-runs the live pipeline over opencode transcripts), `doctor` (environment validation + SurrealDB stats + Markdown report), `finalize` (manual single-session drain trigger).

## Test coverage

| Tier        | Command   | Scope                                                          | Tests | Warm time |
|-------------|-----------|----------------------------------------------------------------|-------|-----------|
| Unit        | cargo tf  | smos-domain + smos-application unit tests only                 | 345   | ~0.6 s    |
| Fast        | cargo t   | Full workspace unit tests + fast integration binaries          | 533   | ~2.6 s    |
| Integration | cargo ti  | Adds every embedded-SurrealDB gate (superset of `cargo t`)     | 643   | ~50 s     |
| External    | cargo tall| Adds Python DeBERTa sidecar + live Ollama                      | 643 + external | 10+ min |

Run the integration tier in CI explicitly:

    cargo test --workspace \
      --features smos-adapters/surrealdb-tests,smos-adapters/slow-tests \
      -- --test-threads=2

The aliases `cargo t` / `cargo ti` / `cargo tall` do NOT override the built-in `cargo test` subcommand — a CI script that runs `cargo test --workspace` directly will silently skip every `e2e_*` binary.

## Quick start

    cargo build --workspace
    cargo run --bin smos -- serve

Send an OpenAI-shaped request:

    curl http://127.0.0.1:8888/v1/chat/completions \
      -H 'content-type: application/json' \
      -d '{"model":"origa:gpt-4o","stream":true,"messages":[{"role":"user","content":"hello"}]}'

## Migration guide

This is the first tagged release — there is no prior version to migrate from.

## Known limitations

- CORS is permissive by default. Safe on `127.0.0.1`; add `[server].allowed_origins` before any non-localhost deploy.
- `find_session` is O(N) `snapshot_all`. Fine for thousands of sessions; a typed SurrealQL fast path is a future slice.
- Vector search cannot push equality filters into the HNSW traversal; they are applied as a Rust-side post-filter.
- Persona injection (`memory_key/persona.md` block) is deferred. The domain builder already accepts `memory_key` for forward compatibility.
- The session watcher's mid-cycle shutdown is bounded by the in-flight cycle. `shutdown_extraction_grace_seconds` bounds the subsequent drain.

## Breaking changes

None (first tagged release).

## Where this is being discussed

- Hacker News: <Show HN URL — fill in after manual submit>
- Reddit: <r/LocalLLaMA post URL — fill in after manual submit>
- dev.to: <article URL — fill in after manual publish>

## Source

- Repository: https://github.com/yurvon-screamo/smos
- License: MIT
```

## Pre-release checklist

- [ ] README badges finalized (CI status, crates.io version, license, Rust MSRV).
- [ ] GitHub Topics set: `ai, memory, llm, semantic-memory, openai-compatible, proxy, rust, self-hosted, agents, local-llm` (via `gh repo edit --add-topic ...`).
- [ ] Crates published (or crate-version decision recorded) before tagging.
- [ ] Tag `v1.0.0` matches the actual published crate version.
- [ ] All three discussion links (HN, Reddit, dev.to) filled in **after** those posts are live.
- [ ] .factcheck.json `gate: READY` confirmed.
