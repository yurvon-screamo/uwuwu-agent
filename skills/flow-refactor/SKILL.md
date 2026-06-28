---
name: flow-refactor
description: Invoke this skill when you need to refactor code.
---

# flow-refactor

Coordinate systematic refactoring — improve readability, maintainability, and compliance with Clean Code principles **without changing external behavior**.

This skill contains refactoring coordination specifics.

## Key Principles

- **Preserve 100% of original behavior** — tests must pass before and after
- **Never make large monolithic changes** — only increments (vertical slices)
- **Never start execution without plan confirmation**

## Clarifying Questions

If necessary, ask the user clarifying questions:

- What specific areas need refactoring?
- Are there known pain points or problem modules?
- Any constraints on the scope of changes?
- Are there areas where tests are missing (critical for safe refactoring)?

## Delegation to Developer

Передай developer'у задачу с типом `refactor`. Входные данные:

- Описание проблемных мест (от пользователя или из анализа)
- Ключевой контекст из оспаривания задачи (Шаг 0)
- Тип задачи: `refactor`

Developer загрузит `architect-refactor` skill, проанализирует код и вернёт валидированный план инкрементального рефакторинга.

## Results (Stages 2–3)

- Summary report of changes made
- If there are no tests — explicitly state this in the report
- Confirmation that behavior has not changed
- Results of review via `code-quality-reviewer`

## Refactoring Verification

In addition to baseline checks:

- Confirm all tests pass before and after changes
- Confirm behavior has not changed (compare outputs / API responses before and after)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original list of problem areas and the refactoring plan so it verifies:
  1. Alignment of implemented changes with the stated problems
  2. Preservation of original behavior (no functional changes)
  3. Compliance with Clean Code Standards (sizes, SRP, naming)
  4. Absence of unnecessary changes outside the plan's scope

---

> **Clean Code Standards** — see skill `rules-clean-code`
