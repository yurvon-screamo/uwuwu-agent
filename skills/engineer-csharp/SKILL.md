---
name: engineer-csharp
description: C# and .NET expertise - async programming, LINQ, DI, generics, Span<T>/Memory<T>, Result pattern, Minimal APIs, System.Text.Json, Serilog.
---

# C# / .NET Expertise

## Strict Rules

### Safety and Typing

- **Nullable Reference Types (NRT)**: Always enabled. Code must not contain potential `NullReferenceException`. Use `?`, `!`, `??` and `default` consciously.
- **Immutability**: Prefer `record` and `readonly struct` for DTOs and simple objects.
- **Explicit Access**: Always specify access modifiers (`private`, `public`, `internal`).

### Performance

- **Async/Await**: NEVER use `.Result` or `.Wait()`. Use `ValueTask` for frequently called methods where the result is often available synchronously.
- **LINQ**: Use only where it doesn't hurt performance in hot loops. Avoid Multiple Enumeration.
- **Collections**: Choose the right type (`List<T>`, `Dictionary<K,V>`, `HashSet<T>`, `ReadOnlySpan<T>`).

### Architecture (SOLID/SRP)

- **DI**: Always use dependency injection through constructors.
- **Interface vs Implementation**: Design from interfaces where necessary for testing or extensibility.
- **Minimal APIs**: For small services, prefer Minimal APIs over controllers.

### Size Limits (strict)

- **Function/Method**: ≤ 60 lines (C# is slightly more verbose than Rust due to braces).
- **Class/File**: MAXIMUM 250 lines.
- If a method is larger — extract logic into private methods or separate services.

### Comments

- **Only "WHY"**: Code should be clear. Comments explain only non-trivial business decisions or architectural hacks.
- **Language**: All comments in ENGLISH.
- **XML Docs**: Only for public interfaces and methods (`/// <summary>`).

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
