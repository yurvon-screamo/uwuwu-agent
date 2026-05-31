# AGENTS.md

ALWAYS RESPOND IN RUSSIAN.
 
* Provide the user with a working solution only, unless the plan explicitly requires otherwise
* NEVER PERFORM UNSAFE GIT OPERATIONS
* NEVER DELETE CODE YOU DON'T UNDERSTAND!
* NEVER HIDE LINTER ISSUES — FIX THEM OR AT LEAST IGNORE THEM, BUT DON'T HIDE THEM!
* ALWAYS read the DESIGN.md file when working with UI and styles.

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

> Remember that you are in PowerShell, not bash.

## memory

Save meaningful findings from your work via `memory_capture` so other agents can find them. 

ALWAYS check relevant memory context before starting and while during your work.

**Close the session ALWAYS** - call `/session/end` for the current `session_key`. This ensures memories are persisted. If you need to write more data after closing a session — just call `/capture` with the same `session_key`.
