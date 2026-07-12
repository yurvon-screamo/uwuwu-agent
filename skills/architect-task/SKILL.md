---
name: architect-task
description: Planning skill for implementation. Methodology for bug/feature/refactor — investigation patterns, root cause analysis, slicing strategies (Prove-It, Contract-First, Baseline-First), and type-specific plan structure. Load BEFORE forming the plan.
---

# architect-task

Планирование реализации. Специфика по типу задачи: bug, feature, refactor. Загружается на фазе PLANNING перед формированием срезов.

## Codebase Investigation (общее)

- Изучи контекст задачи и найди релевантный код
- Исследуй git history затрагиваемых файлов для понимания контекста
- Если нужна информация о внешних библиотеках/зависимостях/best-практикам — исследуй веб
- Определи integration points и существующие паттерны проекта
- Для каждого изменяемого модуля выполнен `grep` импортов — все consuming-файлы найдены

## Тип-специфичная методология

### bug → Prove-It

**Root Cause Determination:**

Determine the **root cause** — clearly articulate why the bug occurs:

- **Обязательно найди способ воспроизвести проблему** — без воспроизведения невозможен Prove-It паттерн
- **If this is expected behavior** → report back to coordinator — task may need reclassification
- **If root cause не найден** → верни `NEEDS_CLARIFICATION` с описанием что проверено и почему не удалось определить причину
- **Only with a confirmed bug** → proceed to planning

**Slice Strategy: Prove-It**

1. **Slice-1: Воспроизведение** — тест, который падает (FAIL). Доказывает что баг существует
2. **Slice-2: Фикс** — минимальное изменение, которое делает тест зелёным (PASS)
3. **Slice-3 (опционально): Улучшения** — если фикс требует рефакторинга окружающего кода

**Anti-patterns:**

- ❌ Фикс без воспроизведения — нет доказательства что баг существовал
- ❌ «Заодно поправлю вот это» — scope expansion вокруг бага
- ❌ Изменение поведения вместо фикса — баг может быть фичей, уточни у руководителя
- ❌ Подавление симптомов вместо root cause — `try/catch` вокруг падающего кода без понимания почему он падает

**Plan Structure (адаптация «Цель работы»):**

```
Problem description: [symptoms]
Root cause: [explanation]
Functional requirements: FR-1: ...
Non-functional requirements: NFR-1: ...
Affected files: `path/to/file.rs` — [what we are changing]
```

**Verification Planning:**

- Slice-1 тест: FAIL → после фикса PASS → все тесты PASS
- Если есть UI: smoke test исправленного сценария
- Регрессионная проверка: все существующие тесты проходят

### feature → Contract-First + Risk-First

**Requirements Formulation:**

- **Functional Requirements (FR):** Specific behaviors, user workflows, data transformations, API contracts
- **Non-Functional Requirements (NFR):** Performance targets, scalability, security, maintainability
- **Architectural Approach:** How the new functionality will be structured, what changes in existing modules, selection of patterns and libraries

**Slice Strategy: Contract-First + Risk-First**

1. **Фундамент** — контракты (типы, интерфейсы, API-сигнатуры). Это позволяет параллелить backend/frontend
2. **Risk-First** — рискованный/неопределённый кусок следующим (fail fast)
3. **Вертикальные срезы** — каждый срез = одно поведение end-to-end

**Anti-patterns:**

- ❌ «Task-1: все модели, Task-2: все API, Task-3: весь UI» — это не срезы, это слои
- ❌ Фича без feature flag — если фича не готова, мержить нельзя
- ❌ Переусложнение — «а давай добавим абстракцию на будущее»
- ❌ Plan without verification strategy — каждый срез должен иметь стратегию проверки

**Plan Structure (адаптация «Цель работы»):**

```
Requirements description: [what exactly needs to be implemented]
Functional requirements: FR-1: ...
Non-functional requirements: NFR-1: ...
Architectural decision: [selection of patterns, libraries, component interactions]
Affected files (new and existing): `path/to/new_file.rs` — [file purpose]
```

**Verification Planning:**

- Автотесты нового (unit ~80% → integration ~15% → e2e ~5%)
- Smoke happy path
- Все существующие тесты PASS
- Если UI: browser smoke

### refactor → Baseline-First

**Code Analysis:**

Focus on violations from `rules-clean-code`:

- Functions with multiple responsibilities (SRP violations)
- Overly long functions/files (exceeding limits)
- Code duplication
- Poor naming
- Redundant comments
- Complex conditions (is an abstraction needed?)

**Pre-Plan Checklist:**

- [ ] For each change, the reason is clear (which violation is being addressed)
- [ ] The task order does not require modifying many files simultaneously
- [ ] Baseline tests exist (or are created as first slice)

**Slice Strategy: Baseline-First**

1. **Slice-1: Baseline** — убедиться что все тесты проходят ДО изменений. Если тестов нет — создать. Это страховка
2. **Incremental refactoring** — один срез = одна логическая группа изменений
3. **Verify after each slice** — тесты проходят, поведение не изменилось

**Anti-patterns:**

- ❌ Рефакторинг без baseline-тестов — нет страховки от регрессий
- ❌ Монолитный рефакторинг — «перепишу весь модуль»
- ❌ Изменение поведения во время рефакторинга — это уже не рефакторинг
- ❌ «Заодно улучшу» — scope creep

**Plan Structure (адаптация «Code Analysis for Refactoring»):**

```
Problem areas: [файл] — [конкретное нарушение: длина, SRP, дублирование]
Strategy: Incremental approach with mandatory user approval.
Notes: [Potential risks, alternative approaches]
```

**Verification Planning:**

- Baseline: все тесты ДО = все тесты ПОСЛЕ
- Поведение не изменилось (сравнение outputs/API responses)
- Если возможно — метрики сложности до/после

## Общие принципы для всех типов

- План состоит из вертикальных срезов (см. формат среза и шаблон полного плана в СПРАВОЧНИКЕ основного агента)
- Уровень детализации — ЧТО, а не КАК
- Scope Discipline: трогай только то, что требует текущий срез
- Simplicity First: что самое простое может сработать?
- Планируй safeguards: feature flags, safe defaults, easy rollback
- Операционные барьеры: One Behavior at a Time, Buildability, E2E-тест среза
