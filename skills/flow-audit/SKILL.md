---
name: flow-audit
description: Invoke this skill when you need to audit the codebase — find code smells, assess technical debt, identify cumulative complexity, detect security vulnerabilities, and produce a prioritized list of issues.
---

# flow-audit

Act as a senior engineer and architect — conduct a systematic codebase audit to identify code smells, technical debt, cumulative complexity, and information security vulnerabilities. The goal is to provide a **complete and objective picture of the codebase health** with prioritized issues. Remediation planning is outside this flow's scope (see `flow-task` → refactor).

> **Checklists live in `rules-*` skills.** This flow orchestrates them — do not duplicate the rules here.
> - Code smells → `rules-clean-code`
> - Security → `rules-security`
> - qlty commands → `rules-qlty`

## Audit Philosophy

Rapid code generation (including by AI agents) without quality control leads to an **accumulation effect**: incorrect patterns become templates for future code. The longer problems remain in the codebase, the more expensive they are to fix — future code will rely on existing patterns, including erroneous ones.

**An audit is an investment in future delivery speed.** Every unaddressed code smell is a landmine under the future foundation.

## Workflow

### Step 1: Context Gathering and Scoping

Before running tools, define the audit boundaries:

- **Define scope**: Entire project, a specific module/directory, or a specific issue?
- **Understand the architecture**: What is the project structure? Key modules and dependencies?
- **Identify the stack**: Languages, frameworks, key dependencies.
- **Identify the attack surface**: APIs, authentication, file handling, external integrations, user input processing?
- **Ask the user**: Specific "pain points"? Known security requirements (compliance, GDPR, PCI-DSS)?

If the user hasn't specified a scope — start by analyzing the entire project, but highlight the key modules.

### Step 2: Tool-Driven Analysis

Use `qlty` (see `rules-qlty` for full command reference):

```bash
qlty check --all                                     # Linting
qlty metrics --all --sort complexity --limit 20      # Top-20 complex files
qlty metrics --functions <file>                      # Function-level metrics for hotspots
qlty smells --all                                    # Code smells (duplication, complexity)
```

**Important**: If `qlty` is not initialized in the project — note this in the report and recommend initialization. Continue the audit with manual analysis.

### Step 3: Expert Review

Based on data from Step 2, conduct an in-depth analysis. **Apply the checklists from `rules-*` skills — do not restate them here.**

#### 3.1 Code Smells → apply `rules-clean-code`

SRP violations, size limits exceeded, naming quality, redundant comments, duplication, magic numbers, deep nesting, dead code, tight coupling. (Full checklist in `rules-clean-code`.)

#### 3.2 Security → apply `rules-security`

Hardcoded credentials, SQL/XSS/Command injections, weak authentication, insecure configs, outdated dependencies with CVEs, IDOR, missing rate-limiting, insecure CORS. (Full checklist in `rules-security`.)

Grep patterns to start with: `password`, `secret`, `api_key`, `token`, `eval()`, `innerHTML`, SQL concatenation, `exec`/`system` calls with variables.

#### 3.3 Technical Debt Analysis (impact matrix)

For each discovered issue, evaluate:

| Criterion | High (3) | Medium (2) | Low (1) |
|---|---|---|---|
| **Change frequency** | Changed every sprint | Changed once a month | Rarely changed |
| **Impact on velocity** | Slows down new feature development | Causes inconvenience | Hardly interferes |
| **Bug risk** | High probability of bugs when changed | Medium probability | Low probability |
| **Accumulation effect** | Problem grows with every new code | Problem is stable | Problem is isolated |

**Priority = Sum of scores × Change frequency**

- **P0 (Critical)**: 10-12 points — fix in the nearest sprint
- **P1 (High)**: 7-9 points — plan for the current sprint
- **P2 (Medium)**: 4-6 points — plan for the next 2-3 sprints
- **P3 (Low)**: 1-3 points — fix on next touch (boy scout rule)

**Security issues scoring ≥ 8 automatically become P0**, regardless of other criteria. Add multipliers: exploitability (no-auth? remote?), data breach impact (PII? financial?).

#### 3.4 Cumulative Complexity Assessment (AI Debt)

Pay special attention to patterns that **scale problems**:

- **Pattern virus**: A bad pattern in a key module that gets copied into new code (agents are especially susceptible — they copy existing styles)
- **Missing tests on critical paths**: Every new code on this path is a potential bug without protection
- **Leaky abstractions**: Interfaces/contracts that can be interpreted ambiguously (especially dangerous for AI agents — they implement things literally)
- **Implicit agreements**: Business rules that are not documented in code but passed on verbally

### Step 4: Delegating Deep Analysis (for Large Projects)

If the project is large — split it into logical blocks and run sub-agents **in parallel**. For small projects, skip this step and proceed to Step 5.

#### Delegation Blocks

- **Block 1**: Core/Domain — business logic and key entities
- **Block 2**: Infrastructure — configuration, DB, external integrations
- **Block 3**: API/Presentation — controllers, routes, serialization
- **Block 4**: Tests — coverage, test quality, missing tests on critical paths
- **Block 5**: Security — authentication, authorization, input handling, cryptography, configuration

#### Sub-agent Prompt

> Conduct a detailed audit of the following codebase block: [Block description, list of files/directories].
>
> Apply checklists from `rules-clean-code` (code smells) and `rules-security` (vulnerabilities). Evaluate each issue using the impact matrix (change frequency, velocity impact, bug risk, accumulation effect).
>
> Check for cumulative complexity: pattern viruses, leaky abstractions, implicit business rules.
>
> **Output format for each discovered issue:**
> - 📍 Location (file:line)
> - 🏷 Issue type (code smell / tech debt / cumulative complexity / 🛡 security vulnerability)
> - 📝 Description
> - 📊 Matrix score (frequency/velocity/risk/accumulation)
> - 🎯 Priority (P0-P3)
> - 💡 Remediation recommendation

### Step 5: Final Synthesis

Combine all data into a final report. Use the format below.

## Output Format

```markdown
# 🔎 Codebase Audit: [Project Name]

## 📊 Summary

| Metric | Value |
|---|---|
| Total codebase size | [number of files / lines] |
| Files with linting errors | [N of M] |
| Code smells (qlty) | [N] |
| Functions > 100 lines | [N] |
| Files > 300 lines | [N] |
| Code duplication | [X%] |
| Top-5 most complex files | [list] |
| 🛡 Security vulnerabilities found | [N] (critical: [N]) |

## 🚨 Critical Issues (P0)

### P0-001: [Issue Title]
- **📍 Location**: `path/to/file:line`
- **🏷 Type**: [Code smell / Tech debt / Cumulative complexity / 🛡 Security vulnerability]
- **📝 Description**: [What is wrong and why it is dangerous]
- **📊 Score**: Frequency=X, Velocity=X, Risk=X, Accumulation=X → Total=X
- **💡 Recommendation**: [Specific remediation steps]
- **⚠️ Risk of inaction**: [What will happen if left unfixed]

---

## 🔶 High Priority Issues (P1)
[Same format for each issue]

---

## 🟡 Medium Priority Issues (P2)
[Grouped by modules/files for navigability]

---

## 🟢 Low Priority Issues (P3)
[Brief list, grouped by issue type]

---

## 📈 Technical Debt Analysis

### Top-5 Hot Spots
Files/modules with the highest concentration of issues.

### Pattern Viruses
Patterns that are being copied and scaling problems.

### Cumulative Complexity
Zones of the codebase where problems reinforce each other.

---

## 🛡 Security Report

### Discovered Vulnerabilities

| # | Severity | Type (OWASP/CWE) | Location | Exploitability |
|---|---|---|---|---|
| 1 | 🔴 Critical | [Type, e.g. CWE-89 SQL Injection] | `file:line` | [Remote without authentication / ...] |

### Risk Zones
- Attack surface, sensitive data, dependencies

### Missing Security Controls
- What is missing — CSP, rate-limiting, input validation, etc.

---

## ✅ What Works Well

- [Positive findings — patterns worth preserving and spreading]
- [This is important for balance and to ensure agents copy good patterns]
```

## Audit Principles

1. **Objectivity**: Rely on `qlty` metrics, not just personal opinion. Numbers > feelings.
2. **Prioritization**: Focus on what **slows down development the most** and **scales problems**.
3. **Cumulative effect**: Pay special attention to problems that grow with every new commit.
4. **Constructiveness**: Every issue comes with a remediation recommendation. An audit is a diagnosis, not a treatment.
5. **Balance**: Note good patterns. Agents need to know what is **right** to copy the best.
6. **Context**: Consider the project stage. More tech debt is acceptable for an MVP than for production.

## References

- `rules-clean-code` — code smell checklist, size limits, naming
- `rules-security` — OWASP/CWE checklist, security red flags
- `rules-qlty` — qlty command reference
