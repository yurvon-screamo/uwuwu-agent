---
name: engineer-csharp
description: C# and .NET expertise - async programming, LINQ, DI, generics, Span<T>/Memory<T>, Result pattern, Minimal APIs, System.Text.Json, Serilog.
---

# C# / .NET Expertise

> Общие правила (размеры, комментарии, SRP, naming) — в `rules-clean-code`. Здесь только C#-специфика.

## Safety and Typing

- **Nullable Reference Types (NRT)**: Always enabled. Code must not contain potential `NullReferenceException`. Use `?`, `!`, `??` and `default` consciously.
- **Immutability**: Prefer `record` and `readonly struct` for DTOs and simple objects.
- **Explicit Access**: Always specify access modifiers (`private`, `public`, `internal`).

## Performance

- **Async/Await**: NEVER use `.Result` or `.Wait()`. Use `ValueTask` for frequently called methods where the result is often available synchronously.
- **LINQ**: Use only where it doesn't hurt performance in hot loops. Avoid Multiple Enumeration.
- **Collections**: Choose the right type (`List<T>`, `Dictionary<K,V>`, `HashSet<T>`, `ReadOnlySpan<T>`).

## Architecture

- **DI**: Always use dependency injection through constructors.
- **Interface vs Implementation**: Design from interfaces where necessary for testing or extensibility.
- **Minimal APIs**: For small services, prefer Minimal APIs over controllers.

## Size Limits (stack-specific)

C# is slightly more verbose than the baseline in `rules-clean-code` due to braces and explicit syntax. Override the baseline with these values:

- **Function/Method**: ≤ 60 lines (vs ≤50 baseline)
- **Class/File**: MAXIMUM 250 lines (vs ≤200 recommended / ≤300 max baseline)
- If a method is larger — extract logic into private methods or separate services.

## Recommended Stack (NuGet)

- **JSON**: `System.Text.Json` (avoid Newtonsoft.Json unless there are specific reasons).
- **Logging**: `Microsoft.Extensions.Logging` or `Serilog`.
- **Validation**: `FluentValidation`.
- **Mapping**: `AutoMapper` or (better) manual mappers/generators.
- **Testing**: `xUnit`, `FluentAssertions`, `Moq` or `NSubstitute`.

## Workflow

Before outputting code, verify:

1. `dotnet build` — no warnings (warnings-as-errors is welcome).
2. `dotnet format` — style is followed (PascalCase for methods/classes, camelCase for parameters).
3. Async is propagated all the way to the top (CancellationToken is supported).

## Output Format

- Only code and concise explanations in English.
- Complete method implementations (no `// ... rest of code`).
