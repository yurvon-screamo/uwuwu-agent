---
name: rules-qlty
description: Run linters, compute code quality metrics, and find code smells using the qlty tool. Use when analyzing code quality, running linters, checking code smells, or when the user mentions qlty.
---

# Qlty Tool Documentation

Always use the `qlty` tool in addition to manual review. It should be run inside a Git repository with Qlty initialized.

## Commands

### `qlty check [OPTIONS] [PATHS]...`

Run linters. By default, only changed files are analyzed.

- `qlty check` — Run on changed files in the current branch
- `qlty check --all` — Run on all files
- `qlty check --all --filter=eslint` — Run only ESLint on all files
- `qlty check web/` — Run on a specific folder

### `qlty metrics [OPTIONS] [PATHS]...`

Compute code quality metrics.

- `qlty metrics --all --max-depth 2` — Metrics summary by directories
- `qlty metrics --all --sort complexity --limit 10` — Overview of the 10 most complex files
- `qlty metrics --functions <file>` — View function-level metrics for a file

### `qlty smells [OPTIONS] [PATHS]...`

Find code smells such as duplication and complexity.

- `qlty smells` — Analyze the current branch
- `qlty smells --all` — Analyze the entire project
- `qlty smells --all --no-duplication` — Skip duplication analysis
- `qlty smells --upstream origin/main` — Analyze relative to a specific branch

## Usage Instructions

1. **Environment check**: Make sure you are in a Git repository with `qlty` initialized.
2. **Command selection**: Choose between `check` (linting), `metrics` (quality scores), or `smells` (complexity/duplication).
3. **Execution**: Run the command using the `Shell` tool.
4. **Output analysis**: Review the tool results and fix any detected issues.
