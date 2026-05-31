---
name: flow-feature
description: Invoke this skill when the user asks to implement new functionality or add a new feature.
---

# flow-feature

Your task is to implement new functionality as described by the user.

It is very important to thoroughly understand the requirements, think through the architectural solution, and ensure high code quality so that the new feature integrates seamlessly into the existing system and is easy to maintain.

**Vague requirements that a human developer might reasonably interpret will be implemented by an agent literally — or, worse yet, creatively.** Therefore, clarify requirements before starting implementation rather than making assumptions on behalf of the user.

This skill contains feature development specifics.

## Feature Requirements Analysis

### Clarifying Questions

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

### Codebase Investigation

- Investigate the existing codebase to understand how the new feature should interact with current components
- If implementing the feature requires a new tool or library — use the `@web-researcher` subagent

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
- Documentation requirements

**Architectural Approach:**
- Clearly articulate how the new functionality will be structured
- What changes will be needed in existing modules
- Selection of patterns and libraries

**Test Cases:**
- Create test cases (unit, integration, UI tests) to verify the new functionality

## Plan Structure

Use the plan template from `tech-lead-architect.md` → "Шаблон полного плана".

Adapt the "Обзор" section to include:

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

## Results (Stages 2–3)

- Summary report on the implemented functionality
- List of created/modified components
- Test results

## Feature Verification

In addition to baseline checks:

- Confirm that the new functionality works according to requirements (via manual testing or running new tests)
- During final validation via `code-quality-reviewer`, pass the sub-agent the original task/requirements so it verifies alignment of the implemented solution with the original task
