---
name: engineer-leptos
description: Apply these rules when developing with Leptos (Rust + WASM framework). Includes reactivity, components, state, SSR/hydrate, and best practices.
---

# Leptos Development Guide

## Philosophy

Leptos is an isomorphic Rust web framework with fine-grained reactivity. No virtual DOM — UI is directly subscribed to signals. Compiles to WebAssembly (client) and native code (server).

## Key Principles

### Safety and Idiomatic Code
- Respect ownership, borrowing, lifetimes
- NEVER use `unsafe`
- NEVER use `regex` — parse manually
- Use `Result`/`Option` for errors

### Performance
- Zero-cost abstractions
- Efficient collections: `Vec`, `HashMap`, `BTreeMap`
- Avoid unnecessary allocations and clones
- `&str` instead of `String` where possible

### Size Limits (strict)
- Recommended function size: ≤ 50 lines
- Maximum function size: 100 lines
- Maximum file size: 200 lines

### Comments
- Only "WHY" — code is self-documenting
- All comments in English
- `///` doc-comments for public API

## Reactivity System

### Signal
```rust
let (count, set_count) = create_signal(0);
// count() — getter (creates subscription)
// set_count.set(1) or set_count.update(|n| *n += 1)
```

**IMPORTANT**: A signal is a function. `count()` subscribes, `count.get()` is a static value.

### Effect
```rust
create_effect(move |_| {
    // Executes when dependencies change
    let val = count.get();
});
```
- Always use `move`
- In SSR, effects do not execute

### Memo
```rust
let doubled = create_memo(move |_| count.get() * 2);
```

### Resource
```rust
let data = create_resource(|| (), |_| async move { fetch_data().await });
// data.read() — data
// data.loading() — loading state
// data.error() — error
```

## Component Model

**IMPORTANT**: Components are setup functions that execute **ONCE**. They do not re-render when state changes.

```rust
#[component]
fn Button(
    #[prop(into)]
    text: String,
    #[prop(optional)]
    variant: ButtonVariant = ButtonVariant::Primary,
) -> impl IntoView {
    view! { <button class={variant.to_string()}>{text}</button> }
}
```

### Reactivity in view!
```rust
// CORRECT — reactive update
view! { <p>{move || count.get()}</p> }

// WRONG — static value
view! { <p>{count.get()}</p> }
```

### Lifecycle
- Setup phase: creation of signals, effects, resources
- Render phase: return `view!`

## Control Flow

Use Leptos components instead of `if`/`match` for reactivity:
- `Show` — conditional rendering
- `For` — lists
- `Suspense` — waiting for async data
- `Transition` — smooth transitions

## Routing
- Use `<A>` instead of `<a>` (no reload)
- `use_params()`, `use_query()`, `use_navigate()`

## State Management

### Local
```rust
let (count, set_count) = create_signal(0);
```

### Context API (preferred for global)
```rust
// Parent
provide_context(MyAppState::new());
// Child
let state = use_context::<MyAppState>().expect("Context not provided");
```

### Prop Drilling
Acceptable for shallow trees — pass `ReadSignal`, not value.

## Error Handling

### Server Functions
```rust
#[server]
async fn my_fn() -> Result<Data, ServerFnError> { ... }
```

### ErrorBoundary
```rust
<ErrorBoundary fallback=|err| view! { <p>"Error: " {err}</p> }>
    <Component />
</ErrorBoundary>
```

## Common Mistakes

1. **Forgetting `move`** — almost every closure in `view!` or `create_effect` requires `move ||`
2. **Passing value instead of signal** — child won't be reactive
3. **Destructuring signal outside tracking** — reactivity is lost
4. **Browser API without check** — use `cfg!(feature = "hydrate")` or `cfg!(target_family = "wasm")`

## Recommended Crates

- **Async**: `tokio`
- **Serialization**: `serde` with derive
- **Logging**: `tracing`
- **Error handling**: `thiserror` (libraries), `anyhow` (applications)

## Workflow

Before submitting: `cargo clippy` → `cargo fmt` → `cargo test`
