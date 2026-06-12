---
name: architect-bug
description: Planning skill for bug fixes. Contains analysis patterns, root cause investigation methodology, and bug-specific plan structure.
---

# architect-bug

Планирование баг-фикса. Специфика: сначала воспроизведение, потом фикс.

## Codebase Investigation

- Study the problem description and find the related code
- Investigate the git history of changes in affected files to understand context
- If understanding the problem requires knowledge of external libraries or dependencies — research the web
- Analyze the code and hypothesize where exactly the error occurs
- **Обязательно найди способ воспроизвести проблему** — без воспроизведения невозможен Prove-It паттерн
- Если root cause не найден — верни руководителю с описанием что проверено и почему не удалось определить причину

### Root Cause Determination

Determine the **root cause** — clearly articulate why the bug occurs:

- **If this is expected behavior** → report back to coordinator — task may need reclassification
- **Only with a confirmed bug** → proceed to planning

## Slice Strategy: Prove-It

Баг-фикс ВСЕГДА следует паттерну Prove-It:

1. **Slice-1: Воспроизведение** — тест, который падает (FAIL). Доказывает что баг существует
2. **Slice-2: Фикс** — минимальное изменение, которое делает тест зелёным (PASS)
3. **Slice-3 (опционально): Улучшения** — если фикс требует рефакторинга окружающего кода

## Anti-patterns

- ❌ Фикс без воспроизведения — нет доказательства что баг существовал
- ❌ «Заодно поправлю вот это» — scope expansion вокруг бага
- ❌ Изменение поведения вместо фикса — баг может быть фичей, уточни у руководителя
- ❌ Подавление симптомов вместо root cause — `try/catch` вокруг падающего кода без понимания почему он падает

## Plan Structure

Используй шаблон из СПРАВОЧНИКА. Адаптируй «Цель работы»:

**Problem description:** [symptoms]

**Root cause:** [explanation]

**Functional requirements:**
- FR-1: [Requirement description]

**Non-functional requirements:**
- NFR-1: Performance: [specific target]
- NFR-2: Security: [specific requirement]

**Affected files:**
- `path/to/file.rs` — [what we are changing]

## Verification Planning

Стратегия верификации для бага:
- Slice-1 тест: FAIL → после фикса PASS → все тесты PASS
- Если есть UI: smoke test исправленного сценария
- Регрессионная проверка: все существующие тесты проходят
