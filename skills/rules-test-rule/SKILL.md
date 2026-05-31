---
name: rules-test-rule
description: Invoke this skill when you need rules for writing tests.
---

# test-rule

Following the principles of Vladimir Khorikov ("Unit Testing Principles, Practices, and Patterns"):

1. **Test behavior, not implementation.** Treat the system under test (SUT) as a "black box".
2. **Resistance to refactoring** — a test should not fail when the internal structure changes without a change in behavior. If a test fails during refactoring (changing structure without changing behavior) — it's a bad test.
3. **Test isolation** — tests are independent, run in parallel.
4. Mocks only for **external** dependencies (network, FS, 3rd-party API). Internal collaborators — real objects, verify state, not interaction.

Your primary metric is **Resistance to Refactoring**.

5. **One concept per test** — a test checks one thing. If a test checks multiple aspects, when it fails, it's unclear what exactly broke.
6. **Test name = specification** — the name should read like a description of the expected behavior in natural language.
7. **A test that never fails is useless** — just as useless as a test that always fails. If a test cannot catch a regression, it's not needed.

## Mocks & Dependencies

Use the "London school" (Mockist) only for external systems. Stick with the "Detroit school" (Classicist) for everything else.

* **Unmanaged Dependencies (SMTP, Message Bus, 3rd Party API):** MUST use Mocks/Stubs.
* **Managed Dependencies (DB, File System):** Use real instances in integration tests. Avoid them in unit tests (extract logic).
* **Collaborators (Internal classes):** NEVER mock. Use real objects. Verify the final result, not interaction (avoid `Verify()` calls on internal class methods).

## Structure (AAA)

Always format tests using the AAA pattern (Arrange, Act, Assert), visually separating them into blocks:

```csharp
// Arrange
var calculator = new Calculator();

// Act
var result = calculator.Sum(1, 2);

// Assert
result.Should().Be(3);
```

## Test Anti-patterns

| Anti-pattern | Problem | Better approach |
|---|---|---|
| Testing implementation details | Fails during refactoring | Test inputs/outputs |
| Blind use of snapshots | No one reviews the diffs | Verify specific values |
| Shared mutable state | Tests pollute each other | Setup/teardown for each test |
| Persistent `test.skip` | Dead code | Remove or fix |
| Too broad assertions | Don't catch regressions | Be specific |
| Missing error handling | Swallowed errors, false passes | Always `await` in async tests |
