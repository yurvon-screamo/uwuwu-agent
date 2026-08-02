# uwuwu-cli

CLI for uwuwu/wiki: semantic search over experience/, task tracker with priority/deadline/designed meta + asset support, access docs per project, projects listing. Rust + SurrealKV + ollama embeddings.

## Setup

```bash
cd uwuwu-cli
cargo build --release
```

Requires:
- Rust toolchain (rustup)
- MSVC build tools
- ollama running on `localhost:11434` with two models:

```bash
ollama pull qwen3-embedding:0.6b                              # semantic search
ollama pull hf.co/SupraLabs/reasoning-summarizer-800m-pre-gguf:Q8_0  # enrich title/description
```

## Install globally

```bash
cargo install --path .
# or copy binary:
cp target/release/uwuwu-cli.exe ~/.local/bin/
```

## Usage

Top-level commands: `wiki`, `projects`, `task`, `access`, `request`, `enrich`.

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

### task — project-scoped task tracker (folder structure + meta)

Tasks live in `projects/<project>/tasks/<task-slug>/README.md` with optional assets in the same folder.

**Frontmatter v2 (mandatory):** `created`, `updated`, `status` (`open | in_progress | blocked | closed`), `priority` (`low | normal | high`), `designed` (bool). Optional: `deadline` (YYYY-MM-DD), `tags`, `title`, `description`.

```bash
uwuwu-cli task list                                              # all non-closed tasks across all projects, grouped
uwuwu-cli task list ckii                                         # non-closed tasks of project ckii
uwuwu-cli task list ckii --status open                           # only open tasks of ckii
uwuwu-cli task list ckii --priority high                         # only high-priority tasks
uwuwu-cli task list ckii --designed true                         # only tasks with architectural work done
uwuwu-cli task list ckii --deadline-from 2026-08-01              # tasks with deadline >= date
uwuwu-cli task search ckii "PDF processing"                  # semantic search
uwuwu-cli task grep ckii "status: blocked"                   # substring search
uwuwu-cli task get ckii ckiiidev-291-oshibka-pri-obrabotke-pdf
uwuwu-cli task worklog ckii ckiiidev-291 "Done: fixed in commit abc"
uwuwu-cli task set-status ckii ckiiidev-291 closed
uwuwu-cli task create ckii new-feature --content "Body..."   # auto-enrich if no title/description; defaults: priority=normal, designed=false
uwuwu-cli task create ckii named-task --content "Body..." --title "T" --description "D" --priority high --deadline 2026-09-01 --designed
uwuwu-cli task edit ckii 291 --content "<full frontmatter + body>"   # full rewrite (create-or-overwrite), `updated` auto-stamped
uwuwu-cli task edit ckii 291 --priority low                    # meta-only update (body preserved)
uwuwu-cli task edit ckii 291 --deadline 2026-12-31 --designed  # multiple meta fields in one call
uwuwu-cli task edit ckii 291 --clear-deadline                  # remove deadline field
uwuwu-cli task clone ckii ckiiidev-291                         # copy task + assets to ./.uwuwu-workspace/tasks/ckiiidev-291/
uwuwu-cli task clone ckii ckiiidev-291 --dest ./my-copy --force  # custom cwd-relative dest, overwrite if exists
uwuwu-cli task asset-get ckii ckiiidev-291 screenshot.png     # print asset (text→stdout, image→info, binary→hint to clone)
```

`task list` without `--status` shows all tasks **except closed**. Sort order: `priority DESC (high→low) → deadline ASC (nulls last) → created ASC`. Without a positional `project` it lists across all projects, grouped by project.

`task edit` modes:
- `--content <full>`: full rewrite (frontmatter + body). Mutually exclusive with meta flags.
- Meta flags only (`--priority`, `--deadline`, `--clear-deadline`, `--designed`, `--no-designed`): targeted frontmatter update, body preserved.
- Empty (neither): error.
- Mixing `--content` with meta flags: error.

`task clone` copies task folder + all assets into a local working directory. Default: `./.uwuwu-workspace/tasks/<slug>/` (avoids collision with `~/.uwuwu` cache). Override via env `UWUWU_TASKS_DIR` (absolute) or `--dest` (cwd-relative). Wiki is the source of truth — no reverse sync.

### access — project-scoped credentials/topology docs

```bash
uwuwu-cli access search rusklimat "postgres"
uwuwu-cli access grep rusklimat "token"
uwuwu-cli access get rusklimat storage-synology
```

### request — change requests for experience/ (staged proposals + queue management)

Change requests are staged proposals for `experience/` edits, queued in `$WIKI_ROOT/.requests/` (default `D:/uwuwu/wiki/.requests`). An AI agent (or human) creates them. The queue is **read-only** from the CLI — there is no `apply`: to act on a request, read its content with `request get` and apply the change manually in your editor, then `request delete` to clear it.

```bash
uwuwu-cli request create create databases/redis.md --content "<frontmatter + body>" --reason "add redis notes"
uwuwu-cli request create update axum.md --content "<new body>" --reason "refresh axum"
uwuwu-cli request create delete obsolete.md --reason "gone"
uwuwu-cli request list                       # id, action, target, reason, created
uwuwu-cli request get <id>                   # full request content (id = filename stem from `list`; prefix match ok)
uwuwu-cli request delete <id>               # discard
```

Requests are a queue of staged proposals. To act on one, `request get` it, apply the change manually in your editor (there are no `wiki edit`/`access edit` commands — the CLI is read-only for `experience/` and access docs), then `request delete` to clear it. IDs are filename stems (timestamped); `get`/`delete` accept the full stem or a unique prefix.

### enrich — LLM-generated title + description

Generates/overwrites `title` + `description` in markdown frontmatter via local Ollama (`reasoning-summarizer-800m` Q8_0). Used for experience articles, access docs, and is invoked automatically by `task create` when `--title`/`--description` are omitted.

```bash
uwuwu-cli enrich experience/databases/                       # каталог
uwuwu-cli enrich experience/databases/redis.md --dry-run     # один файл, без записи
```

Output is deterministic (`temperature: 0`, `seed: 42`). Model receives the article body as a raw prompt (`raw: true` — bypasses chat template, required by this completion-style model).

## Architecture

```
uwuwu-cli/
  Cargo.toml
  src/
    lib.rs               # all modules + run() — pub so integration tests can reach usecases
    main.rs              # thin binary entry: fn main() { uwuwu_cli::run() }
    chunk.rs             # split articles by ## headings, frontmatter parser/writer (TaskMeta: priority/deadline/designed)
    embed.rs             # ollama /api/embed client (reqwest)
    enrich.rs            # ollama /api/generate client + frontmatter title/description writer
    cache.rs             # SurrealKV chunk cache (mtime-based incremental)
    search.rs            # cosine similarity, threshold, dedup, search_with_filter, reindex_file
    grep.rs              # substring (literal) search
    get.rs               # read document body by slug (frontmatter stripped, recursive resolve)
    request.rs           # change-request queue: create/list/get/delete (RequestSub + ops)
    request_util.rs      # request frontmatter parsing, id resolution, slugify/timestamp helpers
    path.rs              # sanitize_project, validate_slug, resolve_existing/within_root, resolve_task_dir/readme (folder structure)
    projects.rs          # list projects with README (count_task_dirs for folder structure)
    access.rs            # access search/grep/get (project-scoped)
    wiki.rs              # WikiSub enum + dispatch
    file_io.rs           # atomic write + line-ending normalize + path-component validation; EditKind/EditOutcome types (shared by task-* write modules)
    task.rs              # TaskSub enum + dispatch (search/list/grep/get/worklog/set_status/create/edit/clone/asset_get)
    task_query.rs        # shared TaskQuery filter + priority_rank + deadline_sort_key (used by list + search)
    task_search.rs       # task semantic search + filters (delegates to task_query)
    task_grep.rs         # task substring (literal) search
    task_list.rs         # task list with filters + sort (delegates to task_query)
    task_list_render.rs  # render task table (Pr/Dl/✎ badges for priority/deadline/designed)
    task_get.rs          # read task README (full content)
    task_create.rs       # create tasks/<slug>/README.md + auto-enrich + meta (priority/deadline/designed)
    task_edit.rs         # write_task_content (full rewrite) + update_task_meta (targeted) + deprecated edit_task wrapper
    task_set_status.rs   # change status + update `updated` field (atomic write)
    task_clone.rs        # clone task folder + assets to local working dir (./.uwuwu-workspace/tasks/<slug>/)
    task_asset.rs        # read/list task assets (text/image/binary, base64 via base64 crate, 5MB limit)
    worklog.rs           # append worklog entry (timestamp auto, atomic write)
    time_util.rs         # format_date_now, format_timestamp_now, days_to_ymd
  tests/
    search_reindex.rs    # integration: reindex_file (skip if no Ollama)
    parity_cores.rs      # parity-lock for extracted read cores (projects/task_list/task_search)
    task_edit.rs         # integration: task edit (create/overwrite, CRLF, worklogs, path-escape)
    request_management.rs # integration: request create/list/get/delete
```

Runtime data (SurrealKV embeddings cache) lives outside the source tree — see `UWUWU_DATA_DIR` in the Env vars table below.

## How search works

1. Split articles into chunks by `## ` headings; prefix with `filename — heading`.
2. Embed chunks via ollama `qwen3-embedding:0.6b`, cache in SurrealKV (mtime-based incremental).
3. Embed query, compute cosine similarity against all chunks.
4. Filter by threshold (0.3), deduplicate by article (keep best chunk score).
5. Return top-N compact list (slug + title + description + score) — NOT full bodies. Use `get`/`task get` for full content.

The pure usecase layer (`search::search`, `grep::grep`, `get::get_document_by_slug`, `request::*`, `path::*`) backs the CLI commands.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `WIKI_ROOT` | `D:/uwuwu/wiki` | Wiki content root (experience/, projects/, _daily/, .requests/) |
| `UWUWU_DATA_DIR` | `~/.uwuwu` | App data dir — holds the SurrealKV embeddings cache (`embeddings.db`) |
| `UWUWU_TASKS_DIR` | _unset (uses `<cwd>/.uwuwu-workspace/tasks/`) | Override `task clone` default destination. Absolute path; if set, `task clone` copies to `<UWUWU_TASKS_DIR>/<slug>/`. |
| `RUST_LOG` | `info` | tracing filter (logs go to stderr) |
