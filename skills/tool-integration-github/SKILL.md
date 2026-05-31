---
name: tool-integration-github
description: "GitHub operations via gh CLI. Manage repos, issues, pull requests, releases, actions, and more. Triggers: 'create issue', 'open PR', 'merge PR', 'list issues', 'GitHub release', 'workflow run', 'repo settings', 'gh search', 'fork repo'. Do NOT use for GitLab or local git operations."
---

# CLI: GitHub (`gh`)

Work with GitHub via the `gh` command-line tool. All operations go through `gh` — no API calls, no scripts.

## Auth

Already authenticated as `yurvon-screamo` with scopes: `gist`, `read:org`, `repo`, `workflow`.

## Repositories

```bash
gh repo clone <owner/repo>
gh repo view <owner/repo>
gh repo list [--limit N] [--public|--private]
gh repo fork <owner/repo>
gh repo create <name> [--public|--private] [--clone]
gh repo delete <owner/repo> --yes
gh repo rename <new-name> [--repo <owner/repo>]
gh repo edit [--description TEXT] [--default-branch NAME] [--repo <owner/repo>]
```

## Issues

```bash
gh issue list [--repo <owner/repo>] [--state open|closed|all] [--label LABEL] [--assignee USER] [--limit N]
gh issue view <NUMBER> [--repo <owner/repo>]
gh issue create --title "TITLE" --body "BODY" [--label LABEL] [--assignee USER] [--repo <owner/repo>]
gh issue edit <NUMBER> [--title TITLE] [--body BODY] [--add-label LABEL] [--remove-label LABEL] [--repo <owner/repo>]
gh issue close <NUMBER> [--repo <owner/repo>]
gh issue reopen <NUMBER> [--repo <owner/repo>]
gh issue comment <NUMBER> --body "COMMENT" [--repo <owner/repo>]
```

## Pull Requests

```bash
gh pr list [--repo <owner/repo>] [--state open|closed|merged|all] [--author USER] [--label LABEL] [--limit N]
gh pr view <NUMBER> [--repo <owner/repo>]
gh pr create --title "TITLE" --body "BODY" [--base BRANCH] [--head BRANCH] [--draft] [--repo <owner/repo>]
gh pr edit <NUMBER> [--title TITLE] [--body BODY] [--repo <owner/repo>]
gh pr merge <NUMBER> [--merge|--squash|--rebase] [--auto] [--repo <owner/repo>]
gh pr close <NUMBER> [--repo <owner/repo>]
gh pr reopen <NUMBER> [--repo <owner/repo>]
gh pr checkout <NUMBER>
gh pr diff <NUMBER> [--repo <owner/repo>]
gh pr review <NUMBER> [--approve|--request-changes|--comment] --body "REVIEW" [--repo <owner/repo>]
gh pr ready <NUMBER> [--repo <owner/repo>]
gh pr checks <NUMBER> [--repo <owner/repo>]
```

## GitHub Actions

```bash
gh run list [--repo <owner/repo>] [--branch BRANCH] [--limit N] [--status success|failure|in_progress]
gh run view <RUN_ID> [--repo <owner/repo>]
gh run view <RUN_ID> --log [--repo <owner/repo>]
gh run rerun <RUN_ID> [--repo <owner/repo>]
gh run cancel <RUN_ID> [--repo <owner/repo>]
gh workflow list [--repo <owner/repo>]
gh workflow view <WORKFLOW> [--repo <owner/repo>]
gh workflow run <WORKFLOW> [--ref BRANCH|TAG] [-f key=value] [--repo <owner/repo>]
gh workflow enable <WORKFLOW> [--repo <owner/repo>]
gh workflow disable <WORKFLOW> [--repo <owner/repo>]
```

## Releases

```bash
gh release list [--repo <owner/repo>] [--limit N]
gh release view <TAG> [--repo <owner/repo>]
gh release create <TAG> [FILES...] --title "TITLE" --notes "NOTES" [--draft] [--prerelease] [--repo <owner/repo>]
gh release edit <TAG> [--title TITLE] [--notes NOTES] [--draft|--prerelease] [--repo <owner/repo>]
gh release delete <TAG> --yes [--repo <owner/repo>]
gh release download <TAG> [--repo <owner/repo>]
```

## Search

```bash
gh search repos <QUERY> [--limit N] [--language LANG] [--sort stars|forks|updated]
gh search issues <QUERY> [--repo <owner/repo>] [--limit N] [--state open|closed]
gh search prs <QUERY> [--repo <owner/repo>] [--limit N] [--state open|closed|merged]
gh search code <QUERY> [--repo <owner/repo>] [--limit N]
```

## Secrets & Variables

```bash
gh secret list [--repo <owner/repo>]
gh secret set <NAME> --body "VALUE" [--repo <owner/repo>]
gh secret delete <NAME> [--repo <owner/repo>]
gh variable list [--repo <owner/repo>]
gh variable set <NAME> --body "VALUE" [--repo <owner/repo>]
gh variable delete <NAME> [--repo <owner/repo>]
```

## API (raw access)

For operations not covered by subcommands:
```bash
gh api <ENDPOINT> [--method GET|POST|PUT|PATCH|DELETE] [-f key=value] [-F key=numeric_value] [--jq EXPR]
```

Examples:
```bash
gh api repos/OWNER/REPO/branches --jq '.[].name'
gh api repos/OWNER/REPO/git/refs -X POST -f sha=ABC123 -f ref=refs/heads/new-branch
```

## Gists

```bash
gh gist create <FILE> [--public] [--description DESC]
gh gist list [--limit N]
gh gist view <ID> [--filename NAME]
gh gist edit <ID> [--filename NAME] [--add FILE]
gh gist delete <ID> --yes
```

## Labels

```bash
gh label list [--repo <owner/repo>]
gh label create <NAME> --color HEX [--description DESC] [--repo <owner/repo>]
gh label edit <NAME> [--name NEW] [--color HEX] [--description DESC] [--repo <owner/repo>]
gh label delete <NAME> --yes [--repo <owner/repo>]
```

## Rules

- ALWAYS specify `--repo <owner/repo>` when working outside the current git repo
- ALWAYS use `--json` with `--jq` for scripted/machine-readable output
- ALWAYS confirm before destructive actions (delete repo, delete release, etc.)
- NEVER expose tokens or credentials in commands
- Prefer `gh` subcommands over raw `gh api` calls when available
- Use `--web` flag to open results in browser when helpful
- Default output format is terminal-friendly; use `--json` for parsing
