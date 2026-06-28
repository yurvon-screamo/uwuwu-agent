---
name: flow-bug
description: Invoke this skill when the user asks to fix a bug or encounters incorrect program behavior.
---

# flow-bug

Your task is to coordinate the bug-fixing process.

**Moving fast only matters if you are moving in the right direction.** A quick fix that does not address the root cause or introduces new problems is movement in the wrong direction. Solve the problem, not its symptoms.

This skill contains bug-fixing coordination specifics.

## Clarifying Questions

If necessary, ask the user clarifying questions:

- Under what conditions does the bug manifest?
- Is this a regression (it used to work) or a bug in new functionality?
- Are there steps to reproduce?
- Expected behavior vs actual behavior?
- Logs, screenshots, trace_id, or other diagnostic information
- Software version / environment

## Delegation to Developer

Передай developer'у задачу с типом `bug`. Входные данные:

- Описание проблемы от пользователя (симптомы, шаги воспроизведения)
- Ответы на уточняющие вопросы
- Ключевой контекст из оспаривания задачи (Шаг 0)
- Тип задачи: `bug`

Developer загрузит `architect-bug` skill, исследует код, определит root cause и вернёт валидированный план.

Если developer не смог определить root cause — уточни у пользователя дополнительную информацию и повтори делегирование.

## Results (Stages 2–3)

- Summary report across all tasks
- Confirmation of bug elimination (reproduction attempt)
- Results of review via `code-quality-reviewer`

## Fix Verification

In addition to baseline checks:

- Confirm the bug is eliminated (reproduction attempt)
- Confirm the fix does not break existing functionality (regression tests)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original bug description, root cause, and fix plan so it verifies:
  1. Alignment of the implemented fix with the original problem
  2. Absence of side effects and regressions
  3. Quality of the test case confirming the fix
