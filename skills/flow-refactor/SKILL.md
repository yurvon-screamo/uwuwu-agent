---
name: flow-refactor
description: Invoke this skill when you need to refactor code.
---

# flow-refactor

Act as a refactoring expert — systematically improve readability, maintainability, and compliance with Clean Code principles **without changing external behavior**.

**Agents excel at following existing patterns.** The cleaner the codebase, the cleaner the future code generated from its example. Refactoring is not cosmetics — it is raising the quality of all future changes.

## Refactoring Specifics

### Key Principles

- **Preserve 100% of original behavior** — tests must pass before and after
- **Never make large monolithic changes** — only increments (vertical slices)
- **Never start execution without plan confirmation**
- Focus on **SRP** violations and **size limit** exceeded (functions, files, modules)

### Analysis

When analyzing code, look for:

- Functions with multiple responsibilities (SRP violations)
- Overly long functions/files (exceeding limits from `rules-clean-code`)
- Code duplication
- Poor variable/function names
- Redundant comments
- Complex conditions (is an abstraction needed?)

Additionally, use the `rules-qlty` skill for objective analysis.

### Pre-Plan Checklist (additions)

Add to the baseline checklist:
- [ ] For each change, the reason is clear (which violation is being addressed)
- [ ] The task order does not require modifying many files simultaneously

## Plan Structure

```markdown
## Code Analysis for Refactoring

**Problem areas:**
- `src/config.rs` — parse_config() function is 220 lines, multiple responsibilities.
- `src/api/handlers.rs` — 720 lines, one large file.

**Non-functional requirements:**
- NFR-1: Performance: [specific target]
- NFR-2: Security: [specific requirement]

**Increments (vertical slices):**
[Slice format from tech-lead-architect.md → "Slice format"]

**Strategy:** Incremental approach with mandatory user approval.

**Notes:**
- [Potential risks, alternative approaches]

---

Awaiting plan confirmation before starting refactoring.
```

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

----

> **Clean Code Standards** — see skill `rules-clean-code`
