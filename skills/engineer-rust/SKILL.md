---
name: engineer-rust
description: Rust expertise - ownership, borrowing, lifetimes, async/await, clippy, rustfmt, serde, tokio, tracing. Apply when working with Rust code.
---

# Rust Development Standards

## Safety

- **NEVER use `unsafe`** — prohibited without exceptions
- **NEVER use the `regex` crate** — use manual parsing or other approaches
- Always follow ownership and borrowing rules
- Explicitly specify lifetime parameters when necessary
- Use `Result`/`Option` for error handling and missing values

## Performance

- Prefer zero-cost abstractions
- Use efficient collections: `Vec`, `HashMap`, `BTreeMap`
- Avoid unnecessary allocations and clones
- Apply iterators and their methods instead of manual loops where appropriate
- Use `&str` instead of `String` where possible

## Architecture and SRP

- One function — one task. If a function does more than one thing — decompose it
- Functions should be small and focused
- Split logic into modules logically

## Size Limits (strict)

- Recommended function size: ≤ 50 lines
- MAXIMUM function size: 100 lines (hard limit)
- MAXIMUM file size: 200 lines (hard limit)
- If a function exceeds the limit — decompose into multiple functions
- If a file exceeds the limit — split into modules

## Comments

- DO NOT write comments explaining "what the code does" — code should be self-documenting
- Comments are acceptable only to explain "WHY" an architectural decision was made
- All comments in ENGLISH
- doc-comments (`///`) are acceptable for public API

## Recommended Crates

- **Async runtime**: tokio
- **Serialization**: serde with derive macros
- **Logging**: tracing (`tracing::info!`, `tracing::debug!`, etc.)
- **CLI**: clap with derive macros
- **Error handling**: thiserror for libraries, anyhow for applications

## Workflow

Before submitting code, always run the check:
1. `cargo clippy` — code must pass without warnings
2. `cargo fmt` — code must be formatted
3. `cargo test` — all tests must pass
