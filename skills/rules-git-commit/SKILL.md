---
name: rules-git-commit
description: Rules for committing and pushing in Git.
---

## Basic Rules

* Commit changes only if the user requested it, do not make this decision yourself.
* If you are not sure which branch to commit to, it is better to ask the user rather than trying to guess.
* If changes were made for different purposes, it is better to commit them as separate commits.

## WORKFLOW

1. **Goal analysis**: Analyze the actual modifications via `git diff`.
2. **Staging**: Stage the required files or all changes (`git add .`).
3. **Message formation**: Create a commit message in the format: `<type-emoji>(<scope>): <subject>`. Emojis and types:
    - ✨ `feature`: new functionality
    - 🐛 `fix`: bug fix
    - 📝 `docs`: documentation
    - 💄 `style`: formatting, missing semicolons, etc. (without changing logic)
    - ♻️ `refactor`: code refactoring
    - ✅ `test`: adding or fixing tests
    - 🔧 `chore`: updating build tasks, package configurations, etc.
4. **Commit and push**: Execute the commit (`git commit -m "..."`) and push the changes to the remote repository (`git push`).

## CRITICAL RESTRICTIONS

**ABSOLUTELY FORBIDDEN:**
- Create new files or directories (outside the context of preparing a commit).
- Fork repositories.
- Use `cherry-pick`, `rebase`, `reset`.
- Delete branches or files.
- Perform any destructive operations or logic changes not related to the staging and commit process.
- Perform any actions that cannot be undone.

**COMMIT AND PUSH ONLY.**

## LANGUAGE RULES

- **Communication**: Respond to the user in the same language they addressed you in.
- **Commit messages**: ALWAYS written strictly in **ENGLISH**.
