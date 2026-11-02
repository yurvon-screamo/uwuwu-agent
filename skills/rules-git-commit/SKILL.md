---
name: rules-git-commit
description: Automatic atomic commits — the agent commits by itself as it works; push only with the PR.
---

## Concept

Commits are automatic and atomic: finished a meaningful step — commit immediately, without asking the user. Monster-commits and piling all task changes into one commit are forbidden.

## When to commit

- **Atom = a completed meaningful plan step**: implementation + its tests, fix + test, refactor, docs edit. One step — one commit.
- The repo in every commit is working: build/lint/tests pass. No WIP or broken states — finish the step to green, then commit.
- Step not finished — don't commit.

## How to commit

1. Stage explicitly: `git add <files of the step>`. Mass staging is forbidden (`git add .`, `git add -A/--all/-u`, `git commit -a`).
2. Message `<type-emoji>(<scope>): <subject>` in English; the subject (after `: `) is ≤72 chars.
   Types: ✨ feature (new functionality) · 🐛 fix · 📝 docs (user-facing docs) · 💄 style (formatting, no logic change) · ♻️ refactor · ✅ test · 🔧 chore. Prompt/policy edits that change agent behavior are ✨ or 🐛, not 📝.
3. `git commit -m "…"`; never bypass pre-commit hooks (`--no-verify` is forbidden).

## Amending

Mistake in your own commits: **before push** — amend or otherwise restructure your own not-yet-pushed commits (the only history-editing exception). **After push** — never amend or rewrite; make a fix commit instead.

## Push

Push is not automatic: only when creating the PR and chasing review fixes.

## Prohibitions

- Direct commits to master/main — work on a feature branch (worktree).
- `rebase`, `reset`, `cherry-pick`, force-push, branch/file deletion, fork — except restructuring your own unpushed commits (see Amending).
- Anything irreversible or destructive beyond staging and committing.
- Secrets, junk (caches/tmp/artifacts), changes outside the current step.
