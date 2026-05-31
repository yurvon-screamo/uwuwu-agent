---
name: flow-audit
description: Invoke this skill when you need to audit the codebase — find code smells, assess technical debt, identify cumulative complexity, detect security vulnerabilities, and produce a prioritized list of issues.
---

# flow-audit

Act as a senior engineer and architect — conduct a systematic codebase audit to identify code smells, technical debt, cumulative complexity, and information security vulnerabilities. The goal is to provide a **complete and objective picture of the codebase health** with prioritized issues. Remediation planning is outside this flow's scope (see e.g. `flow-refactor`).

## Audit Philosophy

### Why This Is Critically Important

Rapid code generation (including by AI agents) without quality control leads to an **accumulation effect**: incorrect patterns become templates for future code. The longer problems remain in the codebase, the more expensive they are to fix — future code will rely on existing patterns, including erroneous ones.

Agents excel at following existing patterns — a codebase full of incorrect patterns will be used as a model for the next function. The cleaner the codebase is now, the cleaner future code will be.

**An audit is an investment in future delivery speed.** Every unaddressed code smell is a landmine under the future foundation.

## Workflow

### Step 1: Context Gathering and Scoping

Before running tools, define the audit boundaries:

- **Define scope**: Entire project, a specific module/directory, or a specific issue?
- **Understand the architecture**: What is the project structure? What are the key modules and their dependencies?
- **Identify the stack**: Languages, frameworks, key dependencies.
- **Identify the attack surface**: Are there APIs, authentication, file handling, external integrations, user input processing?
- **Ask the user**: Are there specific "pain points" they are concerned about? Are there known security requirements (compliance, GDPR, PCI-DSS, etc.)?

If the user hasn't specified a scope — start by analyzing the entire project, but highlight the key modules.

### Step 2: Tool-Driven Analysis

Run `qlty` tools to obtain objective data. Execute commands sequentially:

#### 2.1. Linting
```bash
qlty check --all
```
Record the number and types of linting errors.

#### 2.2. Complexity Metrics
```bash
qlty metrics --all --sort complexity --limit 20
```
Identify the top-20 most complex files. For each problematic file:
```bash
qlty metrics --functions <file>
```
Identify violating functions (exceeding limits from `rules-clean-code`).

#### 2.3. Code Smells
```bash
qlty smells --all
```
Record duplication, excessive complexity, and other smells.

**Important**: If `qlty` is not initialized in the project — note this in the report and recommend initialization. Continue the audit with manual analysis.

### Step 3: Expert Review

Based on data from Step 2, conduct an in-depth analysis. For each problematic file/module:

#### 3.1. Code Smell Checklist (per Clean Code Standards)

- **SRP violations**: Functions/classes with multiple responsibilities
- **Size limit exceeded**:
  - Functions > 50 lines (recommended) / > 100 lines (maximum)
  - Files > 300 lines
- **Naming quality**: Non-obvious variable, function, or class names
- **Redundant comments**: Comments describing "what the code does" instead of "why"
- **Duplication**: Repeated logic that should be abstracted
- **Magic numbers**: Hardcoded values without named constants
- **Deep nesting**: > 3 levels of if/for/while
- **Dead code**: Unused functions, variables, imports
- **Tight coupling**: Modules that cannot be modified independently

#### 3.2. Technical Debt Analysis

For each discovered issue, evaluate using the **impact matrix**:

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

#### 3.3. Security Review

In parallel with quality analysis, check for **information security vulnerabilities**. Many of these are also gross Clean Code violations, but some are not obvious without targeted inspection.

##### Security Checklist (based on OWASP / CWE)

**Authentication and Authorization:**
- Hardcoded credentials (passwords, API keys, tokens, secrets) in code or configs
- Weak authentication mechanisms (missing rate-limiting, simple default passwords)
- Missing access control checks (authorization bypass) on endpoints
- Sessions without expiration, insecure session token storage

**Input Handling and Injections:**
- SQL injections (dynamic query construction without parameterization)
- XSS (reflected / stored / DOM-based) — unescaped output of user data
- Command injection — passing user input to shell/exec
- Path traversal — file access via user-supplied paths without sanitization
- Deserialization of untrusted data

**Data and Cryptography:**
- Sensitive data in plaintext (logs, DB, API responses)
- Weak or outdated cryptographic algorithms (MD5, SHA1 for passwords, DES, ECB mode)
- Missing HTTPS / insecure data transmission
- Insecure secret storage (env files in the repository, .env in VCS)

**Infrastructure and Configuration:**
- Debug mode in production (stack traces, debug endpoints)
- Exposed admin panels, Swagger/UI without authorization
- Insecure CORS policies (`Access-Control-Allow-Origin: *`)
- Missing security headers (CSP, HSTS, X-Frame-Options)
- Outdated dependencies with known CVEs

**Application Logic:**
- Insecure Direct Object Reference (IDOR) — accessing objects by ID without owner verification
- Race conditions on financial / critical operations
- Mass assignment — automatic binding of request fields to models without a whitelist
- Business logic bypass — circumventing business rules through parameter manipulation

##### How to Check

1. **Grep patterns**: Search for typical vulnerable constructs in the code:
   - Hardcoded secrets: `password`, `secret`, `api_key`, `token`, `credential` in literals
   - SQL concatenation: string operations before SQL queries
   - exec/system calls with variables
   - `eval()`, `innerHTML`, `dangerouslySetInnerHTML`
2. **Dependency analysis**: Check `package.json`, `Cargo.toml`, `.csproj`, etc. for outdated versions with known CVEs
3. **Contract verification**: Every API endpoint must have authentication and authorization checks

##### Prioritizing Security Issues

Security vulnerabilities are evaluated using the **same impact matrix**, but with an additional multiplier:

- **Exploitability** (can the exploit be executed without authentication? from the internet?) → raises priority
- **Data breach impact** (what data is compromised? PII? financial?) → raises priority
- Any security issue scoring ≥ 8 automatically becomes **P0**, regardless of other criteria

#### 3.4. Cumulative Complexity Assessment (AI Debt)

Pay special attention to patterns that **scale problems**:

- **Pattern virus**: A bad pattern in a key module that gets copied into new code (agents are especially susceptible to this — they copy existing styles)
- **Missing tests on critical paths**: Every new code on this path is a potential bug without protection
- **Leaky abstractions**: Interfaces/contracts that can be interpreted ambiguously (especially dangerous for AI agents — they implement things literally)
- **Implicit agreements**: Business rules that are not documented in code but passed on verbally

### Step 4: Delegating Deep Analysis (for Large Projects)

If the project is large — split it into logical blocks and run sub-agents **in parallel**. For small projects, skip this step and proceed to Step 5.

#### Delegation Blocks:
- **Block 1**: Core/Domain — business logic and key entities
- **Block 2**: Infrastructure — configuration, DB, external integrations
- **Block 3**: API/Presentation — controllers, routes, serialization
- **Block 4**: Tests — coverage, test quality, missing tests on critical paths
- **Block 5**: Security — authentication, authorization, input handling, cryptography, configuration

#### Sub-agent Prompt:
> Conduct a detailed audit of the following codebase block: [Block description, list of files/directories].
> 
> **qlty metrics context**: [Insert relevant metrics]
> 
> **Analysis criteria:**
> 1. **Code smells**: Duplication, SRP violations, size limit exceeded (function ≤ 50 lines, file ≤ 300 lines), bad names, dead code.
> 2. **Technical debt**: Evaluate using the matrix (change frequency, velocity impact, bug risk, accumulation effect).
> 3. **Cumulative complexity**: Are there pattern viruses? Leaky abstractions? Implicit business rules?
> 4. **Testability**: Are there enough tests? Which critical paths are uncovered?
> 5. **Security**: Hardcoded secrets, SQL/XSS/Command injections, weak authentication, insecure configs, outdated dependencies with CVEs. Check grep patterns: `password`, `secret`, `api_key`, `eval()`, `innerHTML`, SQL concatenation.
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

### Final Audit Report

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
Files/modules with the highest concentration of issues:

1. **`path/to/file`** — [Why this is a hot spot, impact assessment]
2. ...

### Pattern Viruses
Patterns that are being copied and scaling problems:

- **[Pattern name]**: [Description, where found, why it is dangerous for AI agents]
- ...

### Cumulative Complexity
Zones of the codebase where problems reinforce each other:

- [Description of cascading effects between multiple issues]

---

## 🛡 Security Report

### Discovered Vulnerabilities

| # | Severity | Type (OWASP/CWE) | Location | Exploitability |
|---|---|---|---|---|
| 1 | 🔴 Critical | [Type, e.g. CWE-89 SQL Injection] | `file:line` | [Remote without authentication / ...] |
| 2 | 🟠 High | ... | ... | ... |
| 3 | 🟡 Medium | ... | ... | ... |

### Risk Zones

- **Attack surface**: [What attack vectors are present — APIs, upload forms, admin panel, websockets, etc.]
- **Sensitive data**: [Where PII, financial data, and secrets are stored and transmitted]
- **Dependencies**: [Are there dependencies with known CVEs, how up-to-date are the versions]

### Missing Security Controls

- [What is missing — CSP, rate-limiting, input validation, security headers, audit logs, etc.]

---

## ✅ What Works Well

- [Positive findings — patterns worth preserving and spreading]
- [This is important for balance and to ensure agents copy good patterns]
```

## Audit Principles

1. **Objectivity**: Rely on `qlty` metrics, not just personal opinion. Numbers > feelings.
2. **Prioritization**: Not all issues are equally important. Focus on what **slows down development the most** and **scales problems**.
3. **Cumulative effect**: Pay special attention to problems that grow with every new commit (pattern viruses, missing tests, leaky abstractions).
4. **Constructiveness**: Every issue comes with a remediation recommendation. An audit is a diagnosis, not a treatment.
5. **Balance**: Note not only problems but also good patterns. Agents (and developers) need to know what is **right** in order to copy the best.
6. **Context**: Consider the project stage. More tech debt is acceptable for an MVP than for a production system.
