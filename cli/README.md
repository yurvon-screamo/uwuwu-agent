# uwuwu-cli

CLI for uwuwu/wiki: semantic search over experience/, access docs per project, projects listing. Rust + SurrealKV + ollama embeddings.

## Setup

```bash
cd uwuwu-cli
cargo build --release
```

Requires:
- Rust toolchain (rustup)
- ollama running on `localhost:11434` with model:

```bash
ollama pull qwen3-embedding:0.6b                              # semantic search
```

## Install globally

```bash
cargo install --path .
# or copy binary:
cp target/release/uwuwu-cli.exe ~/.local/bin/
```

## Usage

Top-level commands: `wiki`, `projects`, `access`, `request`.

### wiki — experience/ only (howto, no project-bound)

```bash
uwuwu-cli wiki search "prometheus metrics timeout"
uwuwu-cli wiki search "kubernetes cronjob" --top 5
uwuwu-cli wiki grep "SurrealKv"
uwuwu-cli wiki get axum                # by slug (filename without .md)
```

Returns compact list (title + description + score + slug) for top-N matches (threshold 0.3). Slug is the filename without extension — use it with `wiki get`. For access docs (credentials, topology) use `access search` instead.

### projects — list all projects with README

```bash
uwuwu-cli projects
```

### access — project-scoped credentials/topology docs

```bash
uwuwu-cli access search rusklimat "postgres"
uwuwu-cli access grep rusklimat "token"
uwuwu-cli access get rusklimat storage-synology
```

### request — change requests for experience/ (staged proposals + queue management)

Change requests are staged proposals for `experience/` edits, queued in `$WIKI_ROOT/.requests/` (default `~/.uwuwu/.requests` если `WIKI_ROOT` не задан). An AI agent (or human) creates them. The queue is **read-only** from the CLI — there is no `apply`: to act on a request, read its content with `request get` and apply the change manually in your editor, then `request delete` to clear it.

```bash
uwuwu-cli request create create databases/redis.md --content "<frontmatter + body>" --reason "add redis notes"
uwuwu-cli request create update axum.md --content "<new body>" --reason "refresh axum"
uwuwu-cli request create delete obsolete.md --reason "gone"
uwuwu-cli request list                       # id, action, target, reason, created
uwuwu-cli request get <id>                   # full request content (id = filename stem from `list`; prefix match ok)
uwuwu-cli request delete <id>               # discard
```

Requests are a queue of staged proposals. To act on one, `request get` it, apply the change manually in your editor (there are no `wiki edit`/`access edit` commands — the CLI is read-only for `experience/` and access docs), then `request delete` to clear it. IDs are filename stems (timestamped); `get`/`delete` accept the full stem or a unique prefix.

## Architecture

```
uwuwu-cli/
  Cargo.toml
  src/
    lib.rs               # all modules + run() — pub so integration tests can reach usecases
    main.rs              # thin binary entry: fn main() { uwuwu_cli::run() }
    chunk.rs             # split articles by ## headings, frontmatter parser/writer
    embed.rs             # ollama /api/embed client (reqwest)
    cache.rs             # SurrealKV chunk cache (mtime-based incremental)
    search.rs            # cosine similarity, threshold, dedup, search_with_filter, reindex_file
    grep.rs              # substring (literal) search
    get.rs               # read document body by slug (frontmatter stripped, recursive resolve)
    request.rs           # change-request queue: create/list/get/delete (RequestSub + ops)
    request_util.rs      # request frontmatter parsing, id resolution, slugify/timestamp helpers
    path.rs              # sanitize_project, validate_slug, resolve_existing/within_root
    projects.rs          # list projects with README
    access.rs            # access search/grep/get (project-scoped)
    wiki.rs              # WikiSub enum + dispatch
    file_io.rs           # atomic write + line-ending normalize + path-component validation
    log_filter.rs        # tracing filter (surrealdb silenced to warn)
    time_util.rs         # format_date_now, format_timestamp_now, days_to_ymd
  tests/
    search_reindex.rs    # integration: reindex_file (skip if no Ollama)
    parity_cores.rs      # parity-lock for extracted read cores
    request_management.rs # integration: request create/list/get/delete
```

Runtime data (SurrealKV embeddings cache) lives outside the source tree — see `UWUWU_DATA_DIR` in the Env vars table below.

## How search works

1. Split articles into chunks by `## ` headings; prefix with `filename — heading`.
2. Embed chunks via ollama `qwen3-embedding:0.6b`, cache in SurrealKV (mtime-based incremental).
3. Embed query, compute cosine similarity against all chunks.
4. Filter by threshold (0.3), deduplicate by article (keep best chunk score).
5. Return top-N compact list (slug + title + description + score) — NOT full bodies. Use `wiki get` / `access get` for full content.

The pure usecase layer (`search::search`, `grep::grep`, `get::get_document_by_slug`, `request::*`, `path::*`) backs the CLI commands.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `WIKI_ROOT` | `~/.uwuwu` | Wiki content root (experience/, projects/, _daily/, .requests/) |
| `UWUWU_DATA_DIR` | `~/.uwuwu` | App data dir — holds the SurrealKV embeddings cache (`embeddings.db`) |
| `RUST_LOG` | `info` | tracing filter (logs go to stderr) |
