---
name: tool-generic-octocode
description: Octocode — indexed codebase search tool. Use for semantic code search by natural language query (`octocode search`), structural AST grep with ast-grep patterns (`octocode grep`), and viewing file signatures (`octocode view`). Triggers: 'search codebase', 'find code', 'semantic search', 'ast grep', 'structural search', 'octocode'. Only covers search, grep, and view subcommands.
---

# Octocode CLI (search / grep / view)

**Prerequisite:** codebase must be indexed first. If octocode commands return no results or errors, do **not** run `octocode index` — fall back to other search tools (`grep`, `find_path`, `read_file`) instead.

## `octocode search` — Semantic natural-language search

```bash
octocode search <QUERIES>... [OPTIONS]
```

| Flag | Values | Default | Description |
|------|--------|---------|-------------|
| `<QUERIES>...` | positional | required | One or more natural language queries. Multiple queries combined for broader results. |
| `-m`, `--mode` | `all`, `code`, `docs`, `text`, `commits` | `all` | Search scope |
| `-f`, `--format` | `cli`, `json`, `md`, `text` | `cli` | Output format |
| `-t`, `--threshold` | `0.0`–`1.0` | config | Similarity threshold; higher = fewer but more relevant |
| `-e`, `--expand` | flag | off | Expand symbols (full function/class definitions) |
| `-d`, `--detail-level` | `signatures`, `partial`, `full` | `partial` | Code context amount |
| `-l`, `--language` | language name | all | Filter by programming language |

```bash
octocode search "user authentication"
octocode search "authentication" "middleware"          # multi-query
octocode search "database connection" --mode code
octocode search "error handling" --mode commits
octocode search "auth" --threshold 0.7
octocode search "auth" --json
octocode search "auth" --md
octocode search "UserService" --expand --detail-level full
octocode search "auth" --detail-level signatures
```

## `octocode grep` — Structural AST pattern search (ast-grep)

```bash
octocode grep <PATTERN> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `<PATTERN>` | AST pattern with metavariables (required) |
| `-l`, `--lang <LANG>` | Language. Auto-detected from extensions if omitted, but explicit is recommended. |
| `-p`, `--paths <PATHS>` | File paths or glob patterns to scope search |
| `-C`, `--context <N>` | Context lines around matches (default: `0`) |
| `-r`, `--rewrite <REWRITE>` | Rewrite template using metavariables |
| `--update-all` | Apply rewrites in-place (requires `--rewrite`) |
| `--json` | JSON output |

**Metavariables:**

| Token | Meaning |
|-------|---------|
| `$NAME` | Single AST node |
| `$$REST` | Zero or more nodes in a sequence |
| `$$$ARGS` | Zero or more function arguments |
| `$_` | Wildcard single node (don't capture) |
| literal | Exact code structure (`return 0`, `x = 1`) |

**Supported languages:** Rust, JavaScript, TypeScript, Python, Go, Java, C/C++, PHP, Ruby, Lua, Bash, CSS, JSON.

```bash
octocode grep '$VAR.unwrap()' --lang rust
octocode grep 'if err != nil { $$$ }' --lang go
octocode grep 'console.log($$$ARGS)' --lang javascript -C 2

# Rewrite (preview first, then --update-all)
octocode grep '$VAR.unwrap()' --lang rust --rewrite '$VAR.expect("reason")'
octocode grep '$VAR.unwrap()' --lang rust --rewrite '$VAR.expect("reason")' --update-all

# Scoped + JSON
octocode grep 'todo!()' --lang rust --paths 'src/**/*.rs' --json
```

**Warning:** `--update-all` modifies files in-place. Always preview without it first.

## `octocode view` — File signatures

```bash
octocode view [FILES]... [OPTIONS]
```

| Flag | Values | Default | Description |
|------|--------|---------|-------------|
| `[FILES]...` | positional | all indexed | Files to view (glob patterns supported) |
| `--format` | `cli`, `json`, `md`, `text` | `cli` | Output format |

```bash
octocode view src/main.rs
octocode view 'src/**/*.rs'
octocode view src/lib.rs src/utils.rs
octocode view src/main.rs --format json
```

## Which command?

- Find code by meaning → **search**
- Find code by AST structure → **grep**
- Overview file signatures → **view**
- Refactor with AST rewrite → **grep --rewrite**
