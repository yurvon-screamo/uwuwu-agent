---
name: engineer-typescript
description: Apply these rules when writing, reviewing, or refactoring TypeScript code. Includes strict typing, modern patterns, best practices, and recommended stack.
---

# TypeScript Engineer Standards

> Общие правила (размеры, комментарии, SRP, naming) — в `rules-clean-code`. Здесь только TypeScript-специфика.

## Typing

- **Strict Mode**: Always enable `"strict": true`. Code must not contain `any` without extreme necessity. Use `unknown`, type guards, and type assertions consciously.
- **Immutability**: Prefer `const`, `readonly` properties, and immutable data structures (ReadonlyArray, ReadonlyMap).
- **Explicit Types**: Always specify types for function parameters and return values, except in obvious cases.
- **Null Safety**: Use strict null checking (`=== null`, `=== undefined`), optional chaining (`?.`), nullish coalescing (`??`).

## Async & Performance

- **Async/Await**: NEVER use nested `.then()` chains without necessity. Prefer `async/await` with `Promise.all()` for parallel operations.
- **Arrays**: Use proper methods (`map`, `filter`, `reduce`) and avoid mutations. For large arrays, prefer lazy evaluation (generators).
- **Memory**: Avoid memory leaks (event listeners, subscriptions). Always unsubscribe in `useEffect` cleanup or `finally` blocks.

## Architecture

- **DI**: Use dependency injection through constructors or function parameters.
- **Interface vs Implementation**: Design from interfaces (types/interfaces) where necessary for testing or extensibility.
- **Modules**: Prefer ES modules (`import/export`) over CommonJS. Use barrel exports (`index.ts`) for convenience.

## Size Limits (stack-specific)

Slightly more lenient than the baseline in `rules-clean-code` due to TypeScript's verbosity (types, generics, declarations):

- **Function/Method**: ≤ 50 lines (baseline)
- **Class/File**: MAXIMUM 250 lines (vs ≤200 recommended / ≤300 max baseline)
- If a method is larger — extract into private methods or separate utilities.

## Recommended Stack (npm)

- **Runtime**: Node.js (LTS) or Bun/Deno.
- **Framework Backend**: Express, Fastify, NestJS, or Hono.
- **Framework Frontend**: React, Vue, Svelte, or Solid.
- **Validation**: Zod or io-ts (runtime type checking).
- **HTTP Client**: fetch API (native) or axios.
- **Testing**: Vitest or Jest, Testing Library.
- **Linting**: ESLint with `@typescript-eslint`, Prettier.
- **Build**: Vite, esbuild, or tsup.

## Best Practices

### Types and Interfaces

- Use `interface` for objects that can be extended or implemented.
- Use `type` for union types, utility types, and complex compositions.
- Prefer `readonly` for props and configurations.
- Use `as const` for literal types and tuple inference.

### Async Patterns

- Always handle errors in async functions (try/catch or .catch()).
- Use `AbortController` for cancelling fetch requests.
- Prefer `Promise.allSettled()` when you need results from all promises.

### React (if applicable)

- Use functional components and hooks.
- Memoization: `useMemo` for heavy computations, `useCallback` for callback props.
- Custom hooks for reusable logic.
- Strict typing of props via interface/type.

### Node.js

- Use `fs/promises` for async file operations.
- Handle `process.on('unhandledrejection')`.
- Use `path.join()` for cross-platform paths.

### Errors and Debugging

- Create custom error classes (extends Error).
- Use discriminated unions for type-safe error handling (Result pattern).
- Log error context, not just the message.
