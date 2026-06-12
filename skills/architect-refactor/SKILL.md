---
name: architect-refactor
description: Planning skill for code refactoring. Contains analysis methodology, incremental refactoring strategies, and refactor-specific plan structure.
---

# architect-refactor

Планирование рефакторинга. Специфика: baseline-тесты, инкрементальность, preserve behavior.

## Code Analysis

Focus on violations from `rules-clean-code`:
- Functions with multiple responsibilities (SRP violations)
- Overly long functions/files (exceeding limits)
- Code duplication
- Poor naming
- Redundant comments
- Complex conditions (is an abstraction needed?)

### Pre-Plan Checklist

- [ ] For each change, the reason is clear (which violation is being addressed)
- [ ] The task order does not require modifying many files simultaneously
- [ ] Baseline tests exist (or are created as first slice)

## Slice Strategy: Baseline-First

1. **Slice-1: Baseline** — убедиться что все тесты проходят ДО изменений. Если тестов нет — создать. Это страховка
2. **Incremental refactoring** — один срез = одна логическая группа изменений
3. **Verify after each slice** — тесты проходят, поведение не изменилось

## Anti-patterns

- ❌ Рефакторинг без baseline-тестов — нет страховки от регрессий
- ❌ Монолитный рефакторинг — «перепишу весь модуль»
- ❌ Изменение поведения во время рефакторинга — это уже не рефакторинг
- ❌ «Заодно улучшу» — scope creep

## Plan Structure

Используй шаблон из СПРАВОЧНИКА. Адаптируй:

**Code Analysis for Refactoring**

**Problem areas:**
- [файл] — [конкретное нарушение: длина, SRP, дублирование]

**Strategy:** Incremental approach with mandatory user approval.

**Notes:**
- [Potential risks, alternative approaches]

## Verification Planning

Стратегия верификации для рефакторинга:
- Baseline: все тесты ДО = все тесты ПОСЛЕ
- Поведение не изменилось (сравнение outputs/API responses)
- Если возможно — метрики сложности до/после
