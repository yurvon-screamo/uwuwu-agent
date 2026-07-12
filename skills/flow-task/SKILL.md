---
name: flow-task
description: Coordinate task implementation by type (bug/feature/refactor). Contains clarifying questions, delegation template to developer, result criteria, and type-specific verification. Load BEFORE planning phase to determine the right flow.
---

# flow-task

Coordinate task implementation. The skill is split by task type — `bug`, `feature`, `refactor`. Load it whenever a task arrives, then follow the matching section.

**Vague requirements that a human developer might reasonably interpret will be implemented by an agent literally — or, worse yet, creatively.** Therefore, clarify requirements before proceeding rather than making assumptions on behalf of the user.

**Moving fast only matters if you are moving in the right direction.** A quick fix that does not address the root cause or introduces new problems is movement in the wrong direction.

## Clarifying Questions

If necessary, ask the user clarifying questions.

### Common

- What is the desired outcome (measurable)?
- Are there constraints on scope, timeline, or stack?
- Are there known constraints (perf, security, compat)?

### bug

- Under what conditions does the bug manifest?
- Is this a regression (it used to work) or a bug in new functionality?
- Are there steps to reproduce?
- Expected behavior vs actual behavior?
- Logs, screenshots, trace_id, or other diagnostic information?
- Software version / environment?

### feature

- What exactly should the feature do?
- Input/output specifications?
- Edge cases and error handling?
- Integration with existing modules?
- Performance expectations?
- UI/CLI/API requirements?
- Security considerations?
- Scale expectations (users, data volume, requests/sec)?
- Timeline constraints?

### refactor

- What specific areas need refactoring?
- Are there known pain points or problem modules?
- Any constraints on the scope of changes?
- Are there areas where tests are missing (critical for safe refactoring)?

## Delegation to Developer

Передай developer'у задачу. Входные данные:

- Описание задачи от пользователя
- Тип задачи: `bug` / `feature` / `refactor`
- Функциональные и нефункциональные требования (из ответов на уточняющие вопросы)
- Ключевой контекст из оспаривания задачи (`flow-challenge-ask`)
- Исходная формулировка задачи пользователем (без пересказа)

Developer загрузит `architect-task` skill, исследует кодовую базу и вернёт валидированный план.

Если developer не смог определить root cause (для бага) — уточни у пользователя дополнительную информацию и повтори делегирование.

## Results

- Summary report on the implemented work
- List of created/modified components
- Test results
- For bug: confirmation of bug elimination (reproduction attempt)
- For refactor: explicit statement if tests are missing + confirmation that behavior has not changed

## Verification

In addition to baseline checks:

### bug

- Confirm the bug is eliminated (reproduction attempt)
- Confirm the fix does not break existing functionality (regression tests)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original bug description, root cause, and fix plan so it verifies:
  1. Alignment of the implemented fix with the original problem
  2. Absence of side effects and regressions
  3. Quality of the test case confirming the fix

### feature

- Confirm that the new functionality works according to requirements (via manual testing or running new tests)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original task/requirements so it verifies alignment of the implemented solution with the original task

### refactor

- Confirm all tests pass before and after changes
- Confirm behavior has not changed (compare outputs / API responses before and after)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original list of problem areas and the refactoring plan so it verifies:
  1. Alignment of implemented changes with the stated problems
  2. Preservation of original behavior (no functional changes)
  3. Compliance with Clean Code Standards (sizes, SRP, naming)
  4. Absence of unnecessary changes outside the plan's scope

---

> **Clean Code Standards** — see skill `rules-clean-code`
