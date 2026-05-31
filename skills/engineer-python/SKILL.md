---
name: engineer-python
description: Python expertise - type hints, async/await, pydantic, dataclasses, pathlib, pytest, ruff, mypy. Apply when working with Python code.
---

# Python Development Standards

## Safety and Typing

- **Type Hints**: All function signatures MUST have type hints for parameters and return values.
- **Strict Typing**: NEVER use `Any` from `typing` — prefer `object`, `Protocol`, or generics.
- **None Safety**: Use `Optional[T]` (or `T | None` in Python 3.10+) consciously. Avoid implicit None returns.
- **Immutability**: Prefer `dataclass(frozen=True)`, `NamedTuple`, or `Sequence` over mutable defaults.
- **Pydantic**: Use `pydantic` for data validation and serialization over plain dicts.

## Performance

- **Async/Await**: Use `asyncio` for I/O-bound operations. NEVER mix sync blocking calls in async code — use `run_in_executor`.
- **Generators**: Prefer generators (`yield`) and `itertools` over materializing large lists.
- **Comprehensions**: Use list/dict/set comprehensions but avoid deeply nested ones (max 2 levels).
- **Pathlib**: ALWAYS use `pathlib.Path` instead of `os.path` or string paths.

## Architecture (SOLID/SRP)

- **DI**: Use constructor injection or function parameters. Avoid global mutable state.
- **Protocols**: Design from `Protocol` or `ABC` for testing and extensibility.
- **Modules**: Keep `__init__.py` clean. Re-export public API explicitly.

## Size Limits (strict)

- **Function/Method**: ≤ 50 lines.
- **Class**: ≤ 150 lines.
- **MAXIMUM file size**: 300 lines.
- If a function exceeds the limit — extract into smaller functions.
- If a file exceeds the limit — split into modules.

## Comments

- **Only "WHY"**: Code should be self-documenting. Comments explain only non-trivial business decisions or architectural compromises.
- **Language**: All comments in ENGLISH.
- **Docstrings**: Use for public API only (`"""` with Args/Returns/Raises sections).

## Recommended Stack (pip/uv)

- **Package Manager**: `uv` (preferred) or `pip`.
- **Linting/Formatting**: `ruff` for both linting and formatting (replaces flake8, isort, black).
- **Type Checking**: `mypy` with `--strict`.
- **Testing**: `pytest` with `pytest-asyncio` for async tests.
- **Data Validation**: `pydantic` (v2) with model validators.
- **HTTP Client**: `httpx` with async support.
- **HTTP Server**: `FastAPI` or `litestar`.
- **CLI**: `typer` or `click`.
- **Logging**: `structlog` or standard `logging` with `dictConfig`.
- **Serialization**: `orjson` for JSON, `pydantic` for model serialization.
- **ORM**: `sqlalchemy` (2.0+ async style).

## Best Practices

### Type Hints

- Prefer `T | None` over `Optional[T]` (Python 3.10+).
- Use `TypeVar` for generic functions, `Generic[T]` for generic classes.
- Use `Final` for constants, `Literal` for string enums.
- Use `TypedDict` for structured dicts, `NamedTuple` for lightweight data.

### Async Patterns

- Always handle exceptions in coroutines (`try/except`).
- Use `asyncio.gather()` for parallel tasks, `asyncio.TaskGroup` (3.11+) for structured concurrency.
- Use `asyncio.timeout()` for timeouts (3.11+).
- NEVER call blocking functions in async code without `await loop.run_in_executor()`.

### Error Handling

- Create custom exception classes inheriting from `Exception`.
- Use `match/case` with exception types (Python 3.11+ exception groups).
- NEVER use bare `except:` — always specify exception type.
- Log error context with `logger.exception()` in except blocks.

### Strings and Encoding

- Use f-strings for string formatting.
- Always specify encoding explicitly when reading/writing files: `Path("file.txt").read_text(encoding="utf-8")`.
- Use `re` module sparingly — prefer `.startswith()`, `.endswith()`, string methods for simple cases.

### Context Managers

- Prefer `with` statement for resource management.
- Use `contextlib.contextmanager` or `contextlib.asynccontextmanager` for custom managers.
- Use `ExitStack` for dynamic context manager composition.

## Workflow

Before submitting code, always run:

1. `ruff check .` — linting must pass without warnings.
2. `ruff format .` — code must be formatted.
3. `mypy --strict .` — no type errors.
4. `pytest` — all tests must pass.
