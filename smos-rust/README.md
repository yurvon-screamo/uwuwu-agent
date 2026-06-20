# SMOS — Rust port

OpenAI-compatible **semantic memory proxy**. SMOS sits between an OpenAI-shaped
client and an OpenAI-compatible upstream (Ollama, OpenAI, OpenRouter, …) and
adds long-term memory to every conversation: it retrieves relevant facts
before the request reaches the LLM, extracts new facts from the response, and
runs an NLI-driven finalize pass that promotes / merges / rejects candidates.

This repository is the Rust port of the reference Python implementation in
[`smos-poc/`](../smos-poc/) — the authoritative spec lives in
[`smos-poc/ТРЕБОВАНИЯ.md`](../smos-poc/ТРЕБОВАНИЯ.md) (Cyrillic filename is
intentional). When this README and the spec disagree, the spec wins.

## Status

**Production-ready — all 8 slices landed.** Native ort + ONNX Runtime NLI is
the only NLI backend; the legacy Python sidecar was removed.

| Slice | Scope |
|-------|-------|
| 1 — domain            | entities, value objects, pure domain logic (no services) |
| 2 — application + storage | ports, DTOs, errors, protocol helpers, SurrealDB adapter |
| 3 — HTTP passthrough  | axum server, reqwest upstream, SSE marker injection |
| 4 — enrichment        | vector retrieval, rerank, memory-block injection (fail-open) |
| 5 — extraction        | Ollama LLM extractor, retry, post-response background spawn |
| 6 — finalize + NLI    | native ort + ONNX Runtime DeBERTa-v3, contradiction/merge pipeline |
| 7 — session watcher   | background drain loop, graceful shutdown, restart budget |
| 8 — import CLI        | `smos import` re-runs the live pipeline over opencode transcripts |

Test surface: **665 tests** in the default suite (`cargo t`); **5 additional
`#[ignore]` tests** that require a 643 MB DeBERTa ONNX download or a
live Ollama server (`cargo tall`). See [Testing](#testing).

## Quick start

```bash
# Build everything (CPU ort inference by default).
cargo build --workspace

# Start the proxy (default upstream: Ollama on http://localhost:11434).
cargo run --bin smos -- serve

# Or import an existing opencode session into memory.
cargo run --bin smos -- import --help
```

The proxy listens on `127.0.0.1:8888` by default. Send OpenAI-shaped
requests:

```bash
curl http://127.0.0.1:8888/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
        "model": "origa:gpt-4o",
        "stream": true,
        "messages": [{"role":"user","content":"hello"}]
      }'
```

`model` may carry a memory-key prefix (`memory_key:real_model`); the prefix is
stripped before the request reaches the upstream. A bare model name maps to the
`shared` memory namespace.

## Architecture

Strict hexagonal / DDD three-crate workspace. The dependency direction is
one-way — `domain ← application ← adapters` — and enforced by the workspace
layout (no `dev-dependencies` shortcuts across layers).

```
smos-rust/
├── smos-domain/         # pure domain (NO IO, NO async runtime)
│   └── entities, value objects only — no services
├── smos-application/    # ports (traits), DTOs, errors, use cases, protocol helpers
│   └── NO IO here either — async fn in trait, runtime-agnostic
└── smos-adapters/       # the ONLY crate where IO happens
    ├── src/storage/           # SurrealStore (embedded RocksDB), SystemClock
    ├── src/upstream/          # ReqwestUpstream, SSE parser, StreamingBuffer
    ├── src/providers/         # OllamaEmbedding/Extractor, LlamaCppReranker
    ├── src/nli/               # NativeNliClassifier (ort + ONNX DeBERTa-v3)
    ├── src/runtime/           # SessionWatcher, ExtractionSupervisor
    ├── src/http/              # axum router, routes, CORS, tracing
    ├── src/cli/               # subcommand runners (serve/import/doctor/finalize)
    └── src/bin/smos.rs        # `smos` unified binary (clap dispatch)
```

**Layering rules**

- `smos-domain` may not depend on tokio, serde_json, or any IO crate.
- `smos-application` declares ports as `async fn` in trait — the trait is
  `Send`-bounded at the adapter call site, not at the port definition, so the
  application layer stays runtime-agnostic.
- `smos-adapters` is the only crate that imports tokio, reqwest, axum,
  surrealdb, etc. Every concrete port implementation lives here.

## Pipeline overview

Every chat-completion request flows through five stages. Stages 4 and 5 run
off the request path — the response always returns to the client as soon as
the LLM stream completes. Stage 2 (enrichment) runs synchronously on the
request path (it must complete before the upstream forward), but its
fail-open contract guarantees no enrichment failure can block or fail the
request.

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
            └─────────────────┘  (native ort + ONNX DeBERTa-v3)
```

## Configuration

`smos.toml` (next to the binary) is layered — sections present in the file
override the built-in defaults; any section omitted falls back to its
canonical default. See [`smos.toml`](smos.toml) for the canonical example and
[`smos-adapters/src/config.rs`](smos-adapters/src/config.rs) for every field.

| Section          | Purpose                                                            |
|------------------|-------------------------------------------------------------------|
| `[surreal]`      | embedded RocksDB path + namespace/database                        |
| `[server]`       | bind host/port, shutdown grace, extraction toggle, log format     |
| `[upstream]`     | OpenAI-compatible LLM endpoint, auth header, timeout              |
| `[ollama]`       | Ollama URL + embedding/extraction model ids                       |
| `[reranker]`     | llama.cpp reranker URL (`/v1/rerank`)                             |
| `[retrieval]`    | top-K initial/final, min_topic_chars, min_confidence              |
| `[merge]`        | cosine candidate-selection threshold for merge detection          |
| `[confidence]`   | base + multi-source/non-contradiction bonuses, accept/pending cut |
| `[nli]`          | contradiction/entailment softmax thresholds                       |
| `[nli_backend]`  | native ort + ONNX Runtime NLI: HF model id + cache directory      |
| `[heat]`         | decay rate, min threshold                                         |
| `[session]`      | timeout, pending overflow threshold, watcher scan interval        |

## Binaries

SMOS ships as a single `smos` binary with four subcommands.

### `smos serve`

The proxy. HTTP server + [`SessionWatcher`] + native NLI classifier.

```bash
# Default: HTTP server + SessionWatcher + native ort NLI.
cargo run --bin smos -- serve
```

Server-mode startup constructs the native classifier once so the watcher can
finalize sessions without paying cold-start latency on the first drain. If the
classifier fails to start, HTTP still serves (chat completions do not need
NLI) — the watcher is skipped and operators restart once the ONNX model is
downloaded / the cache directory is writable.

### `smos import`

Re-runs the live extraction pipeline over an opencode session transcript and
writes the resulting facts to the store. Useful for backfilling memory from
historical sessions.

```bash
# Discover + fetch a session from a running opencode server.
cargo run --bin smos -- import ses_abc123 --memory-key origa

# List discoverable sessions.
cargo run --bin smos -- import --list

# Import a pre-exported transcript JSON (no opencode server needed).
cargo run --bin smos -- import --from-file ./session.json --memory-key origa

# Parse + print turns only — no models, no writes.
cargo run --bin smos -- import ses_abc123 --dry-run

# Restrict to specific agents / apply pagination.
cargo run --bin smos -- import ses_abc123 --agent engineer --offset 5 --limit 10
```

Flags: `--memory-key`, `--port`, `--agent` (repeatable), `--limit`, `--offset`,
`--dry-run`, `--list`, `--from-file`, `--config`.

### `smos doctor`

Environment validation + SurrealDB stats + Markdown report. Runs the
configured check matrix (binaries, Ollama, SurrealDB, optional reranker) and
prints a colourised summary.

```bash
# Full check matrix.
cargo run --bin smos -- doctor

# SurrealDB stats only (fast, no model round-trips).
cargo run --bin smos -- doctor --stats

# Write a Markdown report to ./smoke_report.md.
cargo run --bin smos -- doctor --report
```

Flags: `--stats`, `--report [<path>]`, `--skip-ollama`,
`--color {always,never,auto}`, `--config`.

### `smos finalize`

Manual single-session drain trigger. Constructs the native NLI classifier,
runs [`FinalizeSession`] against the session's pending facts, prints the
aggregate stats. Used as a smoke-test entry point — the production watcher
wraps the same use case with a polling loop instead.

```bash
# Scoped finalize (fast, one namespace).
cargo run --bin smos -- finalize sess_<12 hex chars> --memory-key origa

# Discovery fallback (scans every namespace the session touched).
cargo run --bin smos -- finalize sess_<12 hex chars>
```

### Global flag

`--config <path>` (global, defaults to `smos.toml`) overrides the config
file for every subcommand.

## Testing

SMOS runs **every test that does NOT carry `#[ignore]`** under the default
`cargo t`. Embedded SurrealDB + wiremock + in-process axum all run without
external services.

| Command | Scope | Tests |
|---------|-------|-------|
| `cargo tf` | `smos-domain` + `smos-application` unit tests only | 351 |
| `cargo t` | Full workspace + every embedded-SurrealDB / wiremock e2e binary | 665 |
| `cargo tall` | Adds the `#[ignore]` tests (native NLI 643 MB model download + any live Ollama test) | 665 + 5 ignored |

### Dev workflow

- **During active coding**: `cargo tf` after every save (super-tight loop).
- **Before commit**: `cargo t` (catches adapter regressions too).
- **Before release**: `cargo tall` (real DeBERTa ONNX model + live Ollama).

### `#[ignore]` semantics

Tests that require either a large model download or a live external service
carry `#[ignore = "<reason>"]`. They stay discoverable (visible in test
output, runnable via `cargo test -- --ignored <name>`) but do not pollute
the default suite.

Current `#[ignore]` inventory:

- `native_nli_tests::*` — 5 tests, require 643 MB DeBERTa-v3 ONNX model
  download.

The previous "pre-existing SurrealDB 2.x regression" markers have been
removed — those were syntax mistakes in our own queries, not engine bugs:

- `array::contains(...)` is not a SurrealQL function. Use the `CONTAINS`
  operator (`WHERE source_sessions CONTAINS $sid`).
- `array::difference(a, b)` is the **symmetric** difference A△B. Use
  `array::complement(a, b)` for the relative complement A\B that the
  pending-bookkeeping and dedup transactions actually need.

### Limiting CPU usage

The `--test-threads` cap is baked into every alias (`--test-threads=4` for
`cargo t` / `cargo tf`, `--test-threads=2` for `cargo tall`). Override per
invocation:

```bash
# PowerShell
$env:RUST_TEST_THREADS = "4"
cargo test

# Bash
RUST_TEST_THREADS=4 cargo test
```

Or pass `-- --test-threads=N` after any alias.

### Raw commands (no alias)

```bash
# Equivalent to `cargo t`.
cargo test --workspace -- --test-threads=4

# Equivalent to `cargo tall`.
cargo test --workspace --include-ignored -- --test-threads=2
```

### Lint gate (CI-equivalent)

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## Known limitations

**Deviations from the spec (documented trade-offs)**

1. **CORS is permissive** — `CorsLayer::permissive()` ships by default for
   browser-driven OpenAI clients. Safe because the default bind is
   `127.0.0.1`; before any non-localhost deploy, add an explicit
   `[server].allowed_origins` field. The startup log warns when the bind is
   non-localhost together with permissive CORS.
2. **`find_session` is O(N) `snapshot_all`** — `FinalizeSession` loads every
   session row and filters in Rust. Fine for thousands of sessions; a future
   slice adds a typed SurrealQL `SELECT … WHERE id = $id` fast path.
3. **`RepoError::QueryFailed` collapses SDK detail** — every SurrealDB error
   is folded into one enum variant with the raw message in a `String`. A
   typed `enum SurrealError` would be cleaner; the SDK does not currently
   expose stable discriminants.
4. **Persona injection deferred** — §3 step 7 / §11
   (`memory_key/persona.md` block) is not yet wired; the domain builder
   already accepts `memory_key` for forward compatibility.
5. **Session watcher shutdown is bounded by the in-flight cycle** — a
   `shutdown_rx` signal that arrives mid-cycle is observed only after the
   cycle completes. The per-cycle latency scales with the pending backlog;
   `shutdown_extraction_grace_seconds` bounds the subsequent drain.

**Inherited POC limitations**

1. Cosine embeddings are **not** used for contradiction / merge detection
   (empirically: avg similarity 0.82 between contradicting pairs, 4/16 above
   0.92). Cosine only feeds candidate selection; DeBERTa NLI owns every
   verdict.
2. Drift walk recall = 100 %, merge recall = 50 % on the POC fixture corpus.
3. Brute-force cosine fallback kicks in whenever HNSW under-serves — full
   table scan with cosine similarity in SurrealQL, slower than the index path.
4. Vector search cannot push equality filters into the HNSW traversal; they
   are applied as a Rust-side post-filter.
5. Ollama `/api/embeddings` is the legacy single-prompt endpoint (still
   functional in 0.x; newer Ollama releases deprecated it).
6. No multi-tenant auth — every memory_key shares one SurrealDB namespace.
7. Extraction pipeline is best-effort — a model timeout logs and drops the
   candidate rather than retrying past the configured attempt budget.

## Production deployment

**Native ort + ONNX Runtime is the only NLI backend.** No Python, no torch,
no subprocess. The first `smos serve` startup downloads the DeBERTa-v3 ONNX
export (~643 MB) into `[nli_backend].cache_dir`. Pre-warm the cache with:

```bash
# Pre-warm the native NLI model cache (downloads ~643 MB).
cargo run --bin smos -- finalize sess_dummy --memory-key shared
```

**GPU acceleration** — opt in via a cargo feature at build time (see
[Native NLI backend](#native-nli-backend-ort--onnx-runtime)). CPU is the
default.

**CORS** — review [Known limitations §1](#known-limitations) before binding
to anything other than `127.0.0.1`. The startup log emits a warning when the
configured `host` is non-localhost together with the permissive layer.

**SurrealDB** — embedded RocksDB. Set `[surreal].path` to a persistent
directory on disk; the proxy creates it on first run. No external database
process is needed.

**Upstream** — point `[upstream].url` at your OpenAI-compatible endpoint.
The proxy forwards the (enriched) request verbatim and injects a session
marker into the response so the next request in the same conversation can be
linked. Streaming (`stream: true`) and non-streaming are both supported.

**Graceful shutdown** — Ctrl+C / SIGTERM triggers a coordinated drain: HTTP
connections finish → in-flight extraction tasks drain (bounded by
`shutdown_extraction_grace_seconds`) → watcher drains pending sessions.
Native NLI is in-process and needs no explicit teardown.

## Native NLI backend (ort + ONNX Runtime)

Native ort + ONNX Runtime is the only NLI backend (the legacy Python sidecar
was removed). The model is the DeBERTa-v3 ONNX export from
`MoritzLaurer/DeBERTa-v3-large-mnli-fever-anli-ling-wanli`.

### GPU support feature flags

Each EP is opt-in via a cargo feature; exactly one GPU feature should be
enabled per build (combining more than one is rarely useful and ort's
prebuilt binary matrix cannot satisfy every combination — e.g. `cuda` and
`webgpu` cannot ship in the same prebuilt build, so enabling both silently
falls back to CPU-only).

| Feature        | Platforms              | GPU                          | When to use                                    |
|----------------|------------------------|------------------------------|------------------------------------------------|
| *(default)*    | all                    | CPU                          | Default — no GPU                               |
| `nli-cuda`     | Windows, Linux         | NVIDIA                       | Best perf for NVIDIA                           |
| `nli-directml` | Windows                | Intel Arc, AMD, NVIDIA       | Universal Windows GPU (DirectX 12)             |
| `nli-metal`    | macOS                  | Apple Silicon                | Mac                                            |
| `nli-webgpu`   | Windows, Linux, macOS  | Universal                    | Cross-platform fallback (Vulkan/DX12/Metal)    |

### Build commands

```bash
# Default (CPU ort inference)
cargo build --release --bin smos

# Windows + Intel Arc (recommended)
cargo build --release --bin smos --features smos-adapters/nli-directml

# Linux + NVIDIA
cargo build --release --bin smos --features smos-adapters/nli-cuda

# Linux + AMD/Intel Arc discrete (via Vulkan)
cargo build --release --bin smos --features smos-adapters/nli-webgpu

# macOS + Apple Silicon
cargo build --release --bin smos --features smos-adapters/nli-metal
```

At startup, `smos serve` logs the detected device (`device=directml`,
`device=cuda`, …) and falls back to CPU automatically if the selected EP
cannot be initialised.

## License

MIT.
