---
name: engineer-rust
description: Rust expertise - ownership, borrowing, lifetimes, async/await, clippy, rustfmt, serde, tokio, tracing. Apply when working with Rust code.
---

# Rust Development Standards

> Общие правила (размеры функций/файлов, комментарии, SRP, naming) — в `rules-clean-code`. Здесь только Rust-специфика.

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

## Size Limits (stack-specific)

Rust is held to a stricter file limit than the baseline in `rules-clean-code` — idiomatic Rust favours small, focused modules:

- **Function**: ≤ 50 lines recommended, ≤ 100 lines hard limit (baseline)
- **File**: MAXIMUM 200 lines **hard limit** (vs ≤200 recommended / ≤300 max baseline — the 300 max does NOT apply to Rust)
- If a file exceeds 200 lines — split into modules.

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
