---
name: engineer-playwright
description: Apply these rules when writing Playwright tests in TypeScript - E2E tests, smoke tests, page objects, fixtures, strict typing.
---

# Playwright Testing Standards

## TypeScript and Typing

- **Strict Mode**: Always enable `"strict": true` in tests. Avoid `any` — use proper Playwright types (`Page`, `Locator`, `BrowserContext`, etc.).
- **Immutability**: Prefer `const` for test data and configurations. Use `readonly` for page object properties where appropriate.
- **Explicit Types**: Properly type test fixtures, custom functions, and page objects. Use built-in Playwright types.
- **Null Safety**: Use optional chaining (`?.`) and nullish coalescing (`??`) when working with optional elements or configurations.

## Test Structure and Performance

- **Async/Await**: ALWAYS use `async/await` in tests. NEVER use raw Promises or `.then()` chains.
- **Parallel Execution**: Design tests as independent and suitable for parallel execution. Avoid shared state between tests.
- **Wait Strategies**: Use Playwright's auto-waiting mechanisms. Avoid arbitrary `waitForTimeout` — use `waitForSelector`, `waitForLoadState`, etc.
- **Memory**: Clean up resources in `afterEach`/`afterAll` hooks. Properly close pages, contexts, and browsers.

## Architecture (Page Object Model)

- **POM Pattern**: Use Page Object Model for better maintainability. Each page/component gets its own class.
- **Encapsulation**: Keep selectors and interaction logic inside page objects. Tests should read like user stories.
- **Reusability**: Create base page objects and utility classes for common functionality (login, navigation).
- **Fixtures**: Use Playwright fixtures for shared setup/teardown and dependency injection.

## Size Limits (strict)

- **Test Case**: ≤ 30 lines (excluding imports and hooks).
- **Page Object Method**: ≤ 20 lines.
- **Page Object File**: MAXIMUM 200 lines.
- If a test is too complex — split it into multiple tests or extract logic into page objects/helpers.

## Comments

- **Only "WHY"**: Test code should be self-documenting. Comments explain only non-trivial test logic or business rules.
- **Language**: All comments in ENGLISH.
- **JSDoc**: Use for public page object methods and test utilities (`/** */` with `@param`, `@returns`).

## Recommended Stack

- **Test Framework**: Playwright Test (latest version)
- **Runtime**: Node.js (LTS version)
- **Language**: TypeScript 5.x+
- **Assertions**: Built-in Playwright `expect` with matchers
- **Reports**: Playwright HTML Reporter, Allure
- **Linting**: ESLint with `@typescript-eslint` and `eslint-plugin-playwright`
- **Formatting**: Prettier

## Playwright Best Practices

### Locators

- Prefer user-facing attributes: `getByRole`, `getByText`, `getByLabel`, `getByPlaceholder`.
- Use `data-testid` only when other options are unsuitable.
- Avoid CSS selectors and XPath — they are unreliable.
- Chain locators for better scoping: `page.locator('.sidebar').getByRole('button')`.

### Assertions

- Use web-first assertions: `await expect(locator).toBeVisible()`.
- Avoid manual assertions: `expect(await locator.isVisible()).toBe(true)`.
- Use soft assertions for non-critical checks: `await expect.soft(locator).toHaveText('...')`.
- Set custom timeouts when needed: `await expect(locator).toBeVisible({ timeout: 10000 })`.

### Page Objects

- Encapsulate all page interactions in page object methods.
- Return `this` for method chaining when appropriate.
- Use getters for frequently used elements.
- Keep page objects focused — one page/component per file.

### Test Organization

- Use `test.describe` to group related tests.
- Use `test.beforeEach` for common setup (navigation, login).
- Use tags for categorization: `test('login @smoke @critical', ...)`.
- Use `test.skip`, `test.only`, `test.fixme` as intended.

### Fixtures

- Create custom fixtures for shared dependencies (authenticated page, test data).
- Use fixtures for setup/teardown logic.
- Combine built-in fixtures with custom ones.

### Error Handling

- Always handle errors in async functions (try/catch or `.catch()`).
- Use `Promise.allSettled()` for parallel operations.
- Log error context with screenshots for debugging.

## Quality Standards

- **Deterministic Tests**: Tests should produce the same result every time.
- **Fast Feedback**: Optimize tests for speed — use appropriate waits.
- **Independent Tests**: Each test should be self-contained.
- **Clear Failures**: Error messages should clearly indicate what failed and why.
- **Maintainability**: Use the POM pattern and keep tests DRY.

## Workflow

Before outputting code, verify:

1. `npm run type-check` or `tsc --noEmit` — for type errors.
2. `npm run lint` — compliance with style and Playwright best practices.
3. Tests are independent and can run in parallel.

## Output Format

- Complete test implementations (no `// ... rest of code`).
- Include necessary imports and configuration.
- Brief explanations in English when writing tests.
- Structured reports when running tests (in the same language as the task).
