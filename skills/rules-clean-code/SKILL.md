---
name: rules-clean-code
description: Apply these rules to ensure code cleanliness, readability, and maintainability. Use when working with code.
---

# Clean Code

Code is read 10× more than it is written. Optimize for the reader, not the writer.

## Decomposition (SRP)

- One function — one responsibility. One module — one reason to change.
- "And"/"Or" in a function name is a smell: `validateAndSave`, `parseOrLog` → split.
- Describe what a function does in one sentence without "and". If you can't, it does too much.

## Size limits (signals, not goals)

When exceeded, decompose — don't rationalize.

| Metric | Recommended | Maximum |
|---|---|---|
| Function | ≤ 50 lines | ≤ 100 lines |
| File | ≤ 200 lines | ≤ 300 lines |

Exception: generated code, or a single cohesive algorithm that cannot be split without leaking internal state. If a file legitimately exceeds the limit, document WHY at the top (`// Single cohesive X parser; splitting would expose internals`). **Noise is never an excuse** — a 400-line file full of redundant comments violates this rule, not satisfies it.

## Naming

Names disclose intent. A reader should understand WHAT without reading the body.

- Semantically precise: `getUserById`, not `getData`. `isAuthenticated`, not `flag`.
- No abbreviations except domain-standard (`id`, `url`, `db`). `usr`, `cfg`, `tmp` are forbidden.
- Booleans: `is`/`has`/`can`/`should` prefix.
- Forbidden words: `data`, `info`, `helper`, `manager`, `processor`, `handler`, `util` — they mean nothing. If the best name is one of these, the abstraction is wrong (see DRY).

## Comments — noise by default

Code with clear names documents itself. A redundant comment HURTS readability — the eye snags on text that adds no information.

**Forbidden:**

- Restating code: `i++; // increment`, `// create user`, `// return result`
- "For readability" on self-documenting code
- Section headers (`// === VALIDATION ===`) — extract a function instead
- Commented-out code (use git)
- Non-English comments

**Allowed ONLY when code cannot explain itself:**

- Magic numbers / non-obvious constants: `MAX_RETRIES = 5 // backpressure from upstream API`
- Complex business conditions — Why, not What
- Non-standard RegExp
- Non-obvious side effects, tradeoffs, external contracts
- Links to issue/ADR/docs: `// see ADR-007 for the caching decision`

**Test:** if a comment can be removed by renaming a variable or function — remove the comment and rename.

## Functions

- Arguments: ≤ 2 ideal, ≤ 3 acceptable, ≥ 4 — refactor (group into an object or split the function).
- No boolean flag arguments: `send(email, true)` → split into `sendNow` / `scheduleSend`. The caller cannot tell what `true` means.
- Single level of abstraction per function. Don't mix `users.save()` with `fs.writeFileSync()` in the same body.
- Fail fast: guard clauses at the top, not deep nested branches. Max nesting depth: 2.

## Components

1 component = 1 file.

## DRY — knowledge, not code

DRY applies to domain knowledge, not identical-looking code. Two identical fragments may model different concepts that evolve independently (e.g. shipping address vs. warehouse address). Merging them couples unrelated change rates.

- Do not extract on the first repetition. Wait for the third occurrence (Rule of Three / AHA — Avoid Hasty Abstractions) and confirm all copies must change together when the underlying rule changes.
- A correct abstraction has a real domain name (`Money`, `TaxRate`, `InvoiceNumber`). If the best name is `Helper`, `Utils`, or `ProcessData`, you are abstracting form, not knowledge.
- Do not cross module boundaries with shared code: two modules keeping their own `Order` model is often correct, even if the shapes look alike.
