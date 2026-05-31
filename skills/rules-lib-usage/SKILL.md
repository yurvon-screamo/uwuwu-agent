---
name: rules-lib-usage
description: Rules for using libraries and frameworks. Apply when writing code with external dependencies — connecting libraries, using framework APIs, choosing patterns. Use when working with any code that depends on specific library versions.
---

# Library Usage

Don't write from memory — verify. Training data becomes outdated, APIs change, patterns get deprecated. Every library API call should be based on up-to-date documentation.

## Always

- Determine exact dependency versions from project files (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, etc.) before writing code
- Verify API currency through official documentation before use
- Use current patterns from documentation, not from memory
- Cite the source for non-trivial decisions — full URL, preferably with an anchor (`/useActionState#usage`, not `/useActionState`)

## Sources (by authority)

1. **Official documentation** — react.dev, docs.djangoproject.com, docs.rs, etc.
2. **Official blog / changelog** — react.dev/blog, nextjs.org/blog
3. **Web standards** — MDN, WHATWG specifications
4. **Compatibility** — caniuse.com, node.green

### Not authoritative sources

- Stack Overflow
- Tutorials and blogs
- AI-generated documentation
- The model's own training data

## Never

- Don't use deprecated APIs, even if they are familiar
- Don't write code from memory for framework-specific patterns without verification
- Don't hide conflicts between documentation and existing code — bring them up for discussion
- Don't invent function signatures — if unsure, verify

## When verification is not needed

- Pure logic not dependent on version (loops, conditions, data structures)
- Renaming variables, fixing typos, moving files
- The user explicitly requests speed over verification

## Documentation and code conflicts

When a discrepancy is found — bring the conflict up for discussion:

```
CONFLICT:
The project code uses useState for form state,
but React 19 recommends useActionState.

Source: https://react.dev/reference/react/useActionState#usage

Options:
A) Modern pattern (useActionState) — matches current docs
B) As in the project (useState) — consistent with current code
→ Which approach do you prefer?
```

## If documentation was not found

Say it directly:

> **UNVERIFIED**: Could not find official documentation for this pattern. Based on training data and may be outdated. Verify before using in production.

Honesty is more valuable than false confidence.

## Red flags

- Framework-specific code written without checking documentation for that version
- "I think" or "I'm sure" instead of a source reference
- Using deprecated APIs because they are familiar
- Library versions not verified before implementation
- Citing Stack Overflow or blogs instead of official documentation
