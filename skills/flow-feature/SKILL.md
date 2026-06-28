---
name: flow-feature
description: Invoke this skill when the user asks to implement new functionality or add a new feature.
---

# flow-feature

Your task is to coordinate the implementation of new functionality.

**Vague requirements that a human developer might reasonably interpret will be implemented by an agent literally — or, worse yet, creatively.** Therefore, clarify requirements before proceeding rather than making assumptions on behalf of the user.

This skill contains feature development coordination specifics.

## Clarifying Questions

If necessary, ask the user clarifying questions:

- What exactly should the feature do?
- Input/output specifications
- Edge cases and error handling
- Integration with existing modules
- Performance expectations
- UI/CLI/API requirements
- Security considerations
- Scale expectations (users, data volume, requests/sec)
- Timeline constraints

## Delegation to Developer

Передай developer'у задачу с типом `feature`. Входные данные:

- Описание фичи от пользователя
- Функциональные и нефункциональные требования (из ответов на уточняющие вопросы)
- Ключевой контекст из оспаривания задачи (Шаг 0)
- Тип задачи: `feature`

Developer загрузит `architect-feature` skill, исследует кодовую базу и вернёт валидированный план.

## Results (Stages 2–3)

- Summary report on the implemented functionality
- List of created/modified components
- Test results

## Feature Verification

In addition to baseline checks:

- Confirm that the new functionality works according to requirements (via manual testing or running new tests)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original task/requirements so it verifies alignment of the implemented solution with the original task
