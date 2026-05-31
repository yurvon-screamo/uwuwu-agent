---
name: flow-agents-md-writer
description: Invoke this skill when you need to create or update AGENTS.md and/or DESIGN.md documentation for a project — extracting conventions, commands, boundaries, and design tokens from the codebase to produce AI-agent-ready documentation.
---

# flow-agents-md-writer

Create or improve AGENTS.md and DESIGN.md files that turn AI coding assistants into "experienced project colleagues." The goal: any AI agent reading these files should work as if they've been on the project for months.

**Every line must earn its place.** Vague instructions are worse than no instructions — agents will follow them literally.

---

## Workflow

### Step 1: Codebase Analysis

Before writing anything, perform a thorough analysis. Study **all** of the following:

1. **Package manager & build system**: `package.json`, `package.yaml`, `pnpm-lock.yaml`, `yarn.lock`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `build.gradle`, `pom.xml`, etc.
2. **Config files**: `.eslintrc`, `.prettierrc`, `tsconfig.json`, `tsconfig.*.json`, `rustfmt.toml`, `.rubocop.yml`, `pytest.ini`, `jest.config.js`, `vite.config.ts`, `webpack.config.js`, `docker-compose.yml`, `Dockerfile`, etc.
3. **Existing documentation**: `README.md`, `CONTRIBUTING.md`, existing `AGENTS.md`, `docs/` folder, inline doc patterns.
4. **Code structure**: Analyze `src/`, `lib/`, `app/`, `tests/`, `__tests__/`, `spec/` for architecture and naming patterns.
5. **CI/CD config**: `.github/workflows/`, `.gitlab-ci.yml`, `Jenkinsfile`, `circle.yml` for build/test commands.
6. **Code patterns**: Study real code files to identify import conventions, error handling, naming, and typing styles.

### Step 2: AGENTS.md Creation

Produce a complete AGENTS.md following the canonical structure below. Target **150–200 lines**.

#### Canonical Structure

```markdown
# AGENTS.md - AI Assistant Guide for [Project Name]

## Project Overview
[1-2 sentences: what the project does, tech stack, high-level architecture]

## Quick Start Commands

### Setup
[exact command]

### Development
[exact command]

### Build
[exact command]

### Testing
# Run all tests
[exact command]

# Run a single test file
[exact command]

# Run tests with coverage
[exact command]

### Linting and Formatting
[exact commands]

## Code Style and Conventions

### Imports
[Convention with code example]

### Naming Conventions
[Specific patterns with examples]

### Formatting
[Prettier/Black/gofmt settings — exact configuration]

### Types and Interfaces
[Patterns with before/after examples]

### Error Handling
[Project-specific error handling pattern with example]

### Comments and Documentation
[When and how to document]

## Project Structure
[key directories with brief descriptions]

## Git Workflow

### Commit Messages
[Format with example]

### Branch Naming
[Convention]

### PR Process
[Steps]

## Critical Boundaries (IMPORTANT!)

### ✅ ALWAYS
- [Specific actions that must always be performed]
- [Include exact commands]

### ⚠️ ASK FIRST
- [Actions requiring user approval]
- [Sensitive files/directories]

### 🚫 NEVER
- [Strict prohibitions]
- [Files/directories that must never be changed]

## Security and Secrets
- [Where secrets are stored]
- [Environment variable conventions]
- [What to never commit]

## Pitfalls and Common Issues
- [Project-specific problems]
- [Things that frequently break]
- [Non-obvious dependencies]

## Deployment
[Brief deployment instructions if applicable]
```

#### Quality Standards

1. **Commands in backticks** — Agents copy them verbatim. Verify mentally against the detected package manager.
2. **Include code examples** — Every style rule needs a concrete example:
   ```typescript
   // ✅ Good
   import { UserService } from '@/services/user';

   // ❌ Bad
   import UserService from '../../../services/user';
   ```
3. **Three-level boundaries** — Your most powerful tool:
   - **ALWAYS**: Unconditional actions (run lint before commits, etc.)
   - **ASK FIRST**: Require user approval (DB migrations, deleting files)
   - **NEVER**: Strict prohibitions (commit secrets, modify core files)
4. **Be specific, not generic**:
   - ❌ "Follow standard conventions"
   - ✅ "Use PascalCase for components, camelCase for utilities"
5. **Define single-test commands** — Critical for iterative development:
   - Jest: `pnpm test -- path/to/test.test.ts`
   - Pytest: `pytest tests/test_file.py::test_function`
   - Cargo: `cargo test test_name`
   - Go: `go test -run TestName ./path/to/package`

#### When AGENTS.md Already Exists

1. Preserve what works
2. Replace vague instructions with concrete ones
3. Add missing sections
4. Verify commands are current (check against `package.json`)
5. Add three-level boundaries if missing
6. Add code examples where only descriptions exist

### Step 3: DESIGN.md Creation (if applicable)

If the project has UI components, also create a DESIGN.md — a design system file that AI agents can read to generate visually consistent UI.

#### 3.1 Project Visual Analysis

- Review UI components for recurring patterns
- Extract color palette from CSS/styles
- Identify fonts and their parameters
- Determine spacing system and border radius
- Capture styles of key components (buttons, inputs, cards)

#### 3.2 YAML Front Matter

YAML block enclosed in `---`. Token structure:

```yaml
---
version: alpha
name: [Design system name]
colors:
  primary: "#1A1C1E"
  secondary: "#6C7278"
typography:
  h1:
    fontFamily: Inter
    fontSize: 48px
    fontWeight: 600
    lineHeight: 1.1
  body-md:
    fontFamily: Inter
    fontSize: 16px
    fontWeight: 400
rounded:
  sm: 4px
  md: 8px
spacing:
  sm: 8px
  md: 16px
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.md}"
---
```

**Important**: Use token references in `{path.to.token}` format for value reuse.

#### 3.3 Markdown Sections

Sections in canonical order:
1. Overview (Brand & Style)
2. Colors
3. Typography
4. Layout (Layout & Spacing)
5. Elevation & Depth
6. Shapes
7. Components
8. Do's and Don'ts

Each section: `##` heading with 2–3 paragraphs of description. Examples:

```markdown
## Overview
Minimalist dark interface for a developer productivity tool.
Clean lines, low visual noise, high information density.

## Colors
- **Primary** (#2665fd): CTA, active states, key interactive elements
- **Surface** (#0b1326): Page backgrounds
- **On-surface** (#dae2fd): Primary text on dark backgrounds
```

#### 3.4 Token Naming Standards

Use standard names for compatibility:
- **Colors**: `primary`, `secondary`, `tertiary`, `neutral`, `surface`, `on-surface`, `error`
- **Typography**: `headline-lg`, `headline-md`, `body-lg`, `body-md`, `label-md`
- **Rounded**: `none`, `sm`, `md`, `lg`, `xl`, `full`

#### 3.5 Validation

After creation, verify:
- YAML syntax correctness
- Required `primary` token exists in colors
- All `{token.reference}` links are valid
- WCAG contrast for backgroundColor/textColor pairs (minimum 4.5:1)
- Sections follow the canonical order

### Step 4: Final Output

Provide the complete file(s) ready to be written to disk. Start with a brief analysis summary of what was discovered, then provide the full file contents.

---

## Key Principles

- **Extract, don't invent**: Pull real patterns from the codebase — don't prescribe idealized ones.
- **Balance precision and flexibility**: Tokens give exact values; prose explains intent.
- **Every line earns its place**: If removing a line doesn't degrade agent performance, remove it.
- **Three-level boundaries**: ALWAYS / ASK FIRST / NEVER is your most powerful tool.
- **Specificity wins**: "Use PascalCase" beats "Follow naming conventions."
- **Living artifact**: Both AGENTS.md and DESIGN.md evolve with the project.

## Common Mistakes

- ❌ Creating tokens "from scratch" instead of extracting from existing code
- ❌ Using descriptive color names (`"Midnight Forest Green"`) instead of systemic ones (`primary`)
- ❌ Forgetting text contrast on backgrounds
- ❌ Duplicating values instead of using token references
- ❌ Mixing different units (px and rem) without a system
- ❌ Vague instructions that agents can interpret multiple ways
- ❌ Missing single-test-file commands (critical for iterative development)

## Useful References

### DESIGN.md
- [Overview](https://stitch.withgoogle.com/docs/design-md/overview/) — what and why
- [Format Specification](https://stitch.withgoogle.com/docs/design-md/specification/) — full token and section schema
- [Linting Rules](https://stitch.withgoogle.com/docs/design-md/linting-rules/) — 8 validation rules
- [CLI Validator](https://stitch.withgoogle.com/docs/design-md/cli/) — `@google/design.md` for validation and export
- [W3C Design Token Format](https://www.designtokens.org/) — token standard
