---
name: architect-feature
description: Planning skill for new features. Contains requirements analysis methodology, slicing strategies, and feature-specific plan structure.
---

# architect-feature

Планирование новых фич. Специфика: контракт-first, вертикальные срезы, risk-first.

## Codebase Investigation

- Investigate the existing codebase to understand how the new feature should interact with current components
- If implementing the feature requires a new tool or library — research the web
- Identify integration points and existing patterns to follow

### Requirements Formulation

**Functional Requirements (FR):**
- Specific behaviors the system must exhibit
- User interactions and workflows
- Data transformations and business logic
- API contracts and interfaces

**Non-Functional Requirements (NFR):**
- Performance targets (latency, throughput)
- Scalability expectations
- Security requirements
- Maintainability standards

**Architectural Approach:**
- How the new functionality will be structured
- What changes will be needed in existing modules
- Selection of patterns and libraries

## Slice Strategy: Contract-First + Risk-First

1. **Фундамент** — контракты (типы, интерфейсы, API-сигнатуры). Это позволяет параллелить backend/frontend
2. **Risk-First** — рискованный/неопределённый кусок следующим (fail fast)
3. **Вертикальные срезы** — каждый срез = одно поведение end-to-end

## Anti-patterns

- ❌ «Task-1: все модели, Task-2: все API, Task-3: весь UI» — это не срезы, это слои
- ❌ Фича без feature flag — если фича не готова, мержить нельзя
- ❌ Переусложнение — «а давай добавим абстракцию на будущее»
- ❌ Plan without verification strategy — каждый срез должен иметь стратегию проверки

## Plan Structure

Используй шаблон из СПРАВОЧНИКА. Адаптируй «Цель работы»:

**Requirements description:** [what exactly needs to be implemented]

**Functional requirements:**
- FR-1: [Requirement description]

**Non-functional requirements:**
- NFR-1: Performance: [specific target]
- NFR-2: Security: [specific requirement]

**Architectural decision:** [selection of patterns, libraries, description of component interactions]

**Affected files (new and existing):**
- `path/to/new_file.rs` — [file purpose]
- `path/to/existing_file.rs` — [what changes are needed]

## Verification Planning

Стратегия верификации для фичи:
- Автотесты нового (unit ~80% → integration ~15% → e2e ~5%)
- Smoke happy path
- Все существующие тесты PASS
- Если UI: browser smoke
