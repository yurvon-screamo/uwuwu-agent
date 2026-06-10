---
name: tool-integration-gitlab
description: "GitLab operations via glab CLI. Manage repos, issues, merge requests, CI/CD pipelines, releases, variables, and more. Triggers: 'gitlab issue', 'gitlab mr', 'gitlab pipeline', 'gitlab ci', 'gitlab release', 'gitlab variable'. Do NOT use for GitHub or local git operations."
---

# CLI: GitLab (`glab`)

Work with GitLab via the `glab` command-line tool. All operations go through `glab` — no API calls, no scripts.

## Auth

```bash
glab auth login [-t TOKEN] [--hostname HOST] [-g ssh|https] [--web] [--device]
glab auth status [-a, --all] [--hostname HOST] [-t, --show-token]
glab auth logout
```

## Global Options

```bash
-R, --repo OWNER/REPO     # Target a different repository
-F, --output text|json    # Output format
--jq EXPR                 # Filter JSON output
-P, --per-page N          # Items per page
```

## Repositories

```bash
glab repo clone [<repo> | -g GROUP] [<dir>] [-- <gitflags>...]
glab repo view [REPO] [-b BRANCH] [-w]
glab repo create [PATH] [-n NAME] [-d DESC] [-g GROUP] [--public|--private|--internal] [--readme]
glab repo fork <REPO> [-c, --clone] [--remote]
glab repo list [-g GROUP] [-a USER] [--public|--private|--internal]
glab repo delete <NAME> [<NAMESPACE>/]
```

## Issues

```bash
glab issue list [-A|--all] [-a USER] [-l LABEL] [-m MILESTONE] [--search STR] [-c, --closed] [-R REPO]
glab issue view <ID> [-c, --comments] [-w]
glab issue create [-t TITLE] [-d DESC] [-a USER] [-l LABEL] [-m MILESTONE] [--due-date DATE] [-y]
glab issue update <ID> [-t TITLE] [-d DESC] [-a USER] [-l LABEL] [-u LABEL] [--lock-discussion]
glab issue close [<ID|URL>]
glab issue reopen [<ID|URL>]
glab issue note <ID> [-m MSG]
glab issue delete <ID>
```

## Merge Requests

```bash
glab mr list [-A|--all] [-a USER] [-r USER] [-l LABEL] [-d, --draft] [-M, --merged] [-c, --closed] [-s BRANCH] [-t BRANCH]
glab mr view [<ID|BRANCH>] [-c, --comments] [-w]
glab mr create [-t TITLE] [-d DESC] [-a USER] [-l LABEL] [-b BRANCH] [-s BRANCH] [--draft] [--reviewer USER] [--push] [-y]
glab mr update [<ID|BRANCH>] [-t TITLE] [-d DESC] [-a USER] [-l LABEL] [-u LABEL] [--draft] [-r, --ready] [--target-branch BRANCH]
glab mr merge [<ID|BRANCH>] [-s, --squash] [-r, --rebase] [-d, --remove-source-branch] [-m MSG] [-y]
glab mr close [<ID|BRANCH>]
glab mr reopen [<ID|BRANCH>]
glab mr checkout [<ID|BRANCH|URL>] [-b BRANCH]
glab mr diff [<ID|BRANCH>] [--raw]
glab mr approve [<ID|BRANCH>] [-s SHA]
glab mr revoke [<ID|BRANCH>]
glab mr rebase [<ID|BRANCH>]
glab mr note create [<ID|BRANCH>] [-m MSG] [--file PATH] [--line N]
```

## CI/CD

```bash
glab ci list [-s STATUS] [-r REF] [--source SRC] [-u USER] [-o FIELD] [--sort DIR]
glab ci view [<BRANCH|TAG>] [-p ID] [-w]
glab ci run [-b BRANCH] [--variables K:V] [--mr] [-w]
glab ci status [-b BRANCH] [-l, --live] [-c, --compact]
glab ci retry [<JOB-ID|NAME>] [-b BRANCH] [-p PIPELINE_ID]
glab ci cancel pipeline <ID>
glab ci cancel job <ID>
glab ci lint [FILE] [--dry-run] [--ref REF]
glab ci trace [<JOB-ID|NAME>]
glab ci artifact <REF> <JOB>
```

## Releases

```bash
glab release list [-R REPO]
glab release view [<TAG>] [-w]
glab release create <TAG> [<FILES>...] [-N NOTES] [-F FILE] [-n NAME] [-r REF] [-m MILESTONE]
glab release delete <TAG> [-y] [-t, --with-tag]
glab release download [<TAG>] [-n PATTERN] [-D DIR]
glab release upload <TAG> [<FILES>...]
```

## Variables

```bash
glab variable list [-g GROUP] [-i, --instance]
glab variable set <KEY> <VALUE> [-d DESC] [-g GROUP] [-m, --masked] [-p, --protected] [-r, --raw] [-s SCOPE] [-t env_var|file]
glab variable get <KEY> [-g GROUP] [-s SCOPE]
glab variable delete <KEY> [-g GROUP] [-s SCOPE]
glab variable update <KEY> <VALUE> [-g GROUP]
glab variable export [-g GROUP]
```

## Labels

```bash
glab label list [-g GROUP]
glab label create -n NAME [-c COLOR] [-d DESC] [-p PRIORITY]
glab label edit --label-id ID [-n NAME] [-c COLOR] [-d DESC]
glab label delete <NAME>
```

## API (raw access)

```bash
glab api <ENDPOINT> [-X METHOD] [-f KEY=VAL] [-F KEY=VAL] [--paginate] [--hostname HOST]
```

Examples:
```bash
glab api projects/:fullpath/members --paginate
glab api projects -X POST -f name=myproject -f visibility=private
glab api graphql -f query='{ currentUser { name } }'
```

## Other

```bash
glab schedule list|create --cron CRON --desc DESC --ref REF|update <ID>|delete <ID>|run <ID>
glab milestone list [--group ID] [--state active|closed] [--search STR]
glab milestone create --title TITLE [--due-date DATE] [--desc DESC]
glab deploy-key list|add [KEYFILE] -t TITLE [-c, --can-push]|delete <ID>
glab token create <NAME> [-S SCOPE] [-A LEVEL] [-D DUR] [-g GROUP]
glab token list [-g GROUP] [-a, --active]
glab token revoke <NAME|ID>
glab token rotate <NAME|ID> [-D DUR]
glab snippet create -t TITLE <FILE> [-f FILENAME] [-v public|internal|private]
```

## Rules

- ALWAYS specify `-R OWNER/REPO` when working outside the current git repo
- ALWAYS use `--output json` with `--jq` for scripted/machine-readable output
- ALWAYS confirm before destructive actions (delete repo, delete release, etc.)
- NEVER expose tokens or credentials in commands
- Prefer `glab` subcommands over raw `glab api` calls when available
- Use `-w, --web` flag to open results in browser when helpful
