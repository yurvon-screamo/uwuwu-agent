# AGENTS.md

ALWAYS RESPOND IN RUSSIAN.
 
* Provide the user with a working solution only, unless the plan explicitly requires otherwise
* NEVER PERFORM UNSAFE GIT OPERATIONS
* NEVER DELETE CODE YOU DON'T UNDERSTAND!
* NEVER HIDE LINTER ISSUES — FIX THEM OR AT LEAST IGNORE THEM, BUT DON'T HIDE THEM!
* ALWAYS read the DESIGN.md file when working with UI and styles.
* Any action that may lead to theoretical data loss must be preceded by creating a backup

Tool usage rules:

* NEVER guess file paths — use `find_path` (glob) to locate files before reading/editing them. No exceptions.
* ALWAYS PROACTIVELY USE SKILLS.
* ALWAYS use SKILLS if there are relevant ones for the task. This is VERY important.
* ALWAYS use relevant MCPs to solve the task. This is VERY important.
* ALWAYS invoke ALL relevant SKILLS. Don't limit yourself to just one if you see other relevant skills. This is VERY important.
* ALWAYS use uv/uvx/uv tool instead of pip for installing packages and python for running scripts.

Code rules:

* Don't write tests for the sake of writing tests.
* When working with git (commit, comments, etc.) ALWAYS use ENGLISH.
* Never write code without types, like `any`, `unsafe`, etc.
* Never write comments unless they are needed.
* ALWAYS PROACTIVELY USE the `qlty` formatting and linting tool after making code changes.

## wiki

Wiki — это **база знаний-инструкций** (howto, gotchas, конфиги, credentials, топология), а НЕ память агентов. Память — отдельная подсистема (memory gateway / `vectors.db`); wiki к ней отношения не имеет.

ALWAYS search in the wiki: call `wiki_search` with a descriptive query to find relevant experience articles (howto, gotchas, configs) and access documents (credentials, topology).

When you learn something new or find outdated info, create a change request via `wiki_request` (create/update/delete). 

- `wiki_search` — semantic search over `experience/` (tech howto) or `access/` (credentials, stands). Returns full article text.
- `wiki_grep` — regex search across all articles. Returns matching lines with file paths and line numbers.
- `wiki_get` — get full article content by path (e.g. `experience/rust/axum.md`).
- `wiki_request` — propose article changes (create/update/delete). Saved as `.requests/*.md` for human review.
