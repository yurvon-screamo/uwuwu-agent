---
name: flow-bug
description: Invoke this skill when the user asks to fix a bug or encounters incorrect program behavior.
---

# flow-bug

Your task is to find and fix the bug the user has encountered.

It is very important to thoroughly investigate the symptoms, determine the root cause, and deliver a quality fix that eliminates the bug without side effects and without breaking existing functionality.

**Moving fast only matters if you are moving in the right direction.** A quick fix that does not address the root cause or introduces new problems is movement in the wrong direction. Solve the problem, not its symptoms.

This skill contains bug-fixing specifics.

## Problem Analysis

### Clarifying Questions

If necessary, ask the user clarifying questions:

- Under what conditions does the bug manifest?
- Is this a regression (it used to work) or a bug in new functionality?
- Are there steps to reproduce?
- Expected behavior vs actual behavior?
- Logs, screenshots, trace_id, or other diagnostic information
- Software version / environment

### Codebase Investigation

- Study the problem description and find the related code
- Investigate the git history of changes in affected files to understand context
- If understanding the problem requires knowledge of external libraries or dependencies — use the `@web-researcher` subagent
- Analyze the code and hypothesize where exactly the error occurs
- **It is HIGHLY DESIRABLE to learn to reproduce the problem** so you can confirm its elimination at the end

### Root Cause Determination

Determine the **root cause** — clearly articulate why the bug occurs:

- **If this is expected behavior** → explain to the user and close the task
- **Only with a confirmed bug** → proceed to planning

### Test Case

Create a test case within the project's existing test framework (if none exist, skip this step).

## Plan Structure

Use the plan template from `tech-lead-architect.md` → "Шаблон полного плана".

Adapt the "Обзор" section to include:

**Problem description:** [symptoms]

**Root cause:** [explanation]

**Functional requirements:**
- FR-1: [Requirement description]

**Non-functional requirements:**
- NFR-1: Performance: [specific target]
- NFR-2: Security: [specific requirement]

**Affected files:**
- `path/to/file.rs` — [what we are changing]

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
