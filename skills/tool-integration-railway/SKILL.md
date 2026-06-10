---
name: tool-integration-railway
description: "Railway operations via railway CLI. Deploy apps, manage projects, services, environments, variables, volumes, databases, domains, and more. Triggers: 'deploy to railway', 'railway deploy', 'railway up', 'railway logs', 'railway service', 'railway variable', 'railway project'. Do NOT use for other cloud platforms (AWS, GCP, Azure, Vercel)."
---

# CLI: Railway (`railway`)

Work with Railway via the `railway` command-line tool. All operations go through `railway` — no API calls, no scripts.

## Auth

```bash
railway login
railway login --browserless                       # Without browser (pairing code)
railway logout
railway whoami [--json]
```

CI tokens:
```bash
RAILWAY_TOKEN=<token>                             # Project-scoped
RAILWAY_API_TOKEN=<token>                         # Account/workspace-scoped
```

## Projects

```bash
railway init [-n, --name NAME] [-w, --workspace ID] [--json]   # alias: new
railway link [-p PROJECT] [-e ENV] [-s SERVICE] [-w WORKSPACE] [--json]
railway unlink [-s SERVICE] [-y]
railway list [--json]                             # alias: ls
railway delete [-p PROJECT] [-y] [--2fa-code CODE] [--json]   # alias: rm
railway status [--json]
railway open [-p, --print]
```

## Deploy

```bash
railway up [-d] [-y] [--new] [-s SERVICE] [-e ENV] [-p PROJECT] [-m MSG] [-c, --ci] [--json]
railway deploy [-t TEMPLATE] [-v KEY=VALUE]       # Template (postgres, mysql, redis, mongo)
railway redeploy [-s SERVICE] [-y] [--json]
railway restart [-s SERVICE] [-y] [--json]
railway down [-s SERVICE] [-e ENV] [-y]
railway deployment list [-s SERVICE] [-e ENV] [--limit N] [--json]
```

## Services

```bash
railway add [-d DATABASE] [-s NAME] [-r REPO] [-i IMAGE] [-v KEY=VALUE] [--json]  # DB types: postgres, mysql, redis, mongo
railway service list|link|delete|status|logs|redeploy|restart [-s SERVICE] [-y] [--json]
railway service scale [REGION=REPLICAS ...] [-s SERVICE] [--json]  # eu-west, us-east, us-west, southeast-asia
railway scale [REGION=REPLICAS ...] [-s SERVICE] [-e ENV] [--json]
```

## Variables

```bash
railway variable list [-s SERVICE] [-e ENV] [-k, --kv] [--json]   # aliases: variables, vars, var
railway variable set <KEY=VALUE> [...] [-s SERVICE] [-e ENV] [--stdin] [--skip-deploys] [--json]
railway variable delete <KEY> [-s SERVICE] [-e ENV] [--json]
```

## Environments

```bash
railway environment list|link [-e ENV] [--json]   # alias: env
railway environment new [-d, --duplicate ENV] [--json]
railway environment delete [-e ENV] [-y] [--2fa-code CODE] [--json]
railway environment edit [-e ENV] [-s SERVICE PATH VALUE] [-m MSG] [--stage] [--json]
railway environment config [-e ENV] [--json]
```

## Local Dev

```bash
railway run <COMMAND> [-s SERVICE] [-e ENV] [--no-local]   # alias: local
railway shell [-s SERVICE] [--silent]
railway dev up|down|clean|configure [-e ENV] [--dry-run]   # alias: develop
```

## Logs & Metrics

```bash
railway logs [-s SERVICE] [-e ENV] [-d|-b] [-n LINES] [-f FILTER] [--latest] [-S SINCE] [-U UNTIL] [--json]
railway metrics [-s SERVICE] [-a, --all] [--cpu|--memory|--network|--volume|--http] [-w, --watch] [--json]
```

## SSH & Database

```bash
railway ssh [-s SERVICE] [-e ENV] [--session NAME] [-i IDENTITY] [-- COMMAND]
railway ssh keys list|add [--key PATH] [--name NAME]|remove|github
railway connect [-e ENV]                          # Interactive DB shell (psql, mysql, redis-cli, mongosh)
```

## Domains, Volumes, Buckets

```bash
railway domain [DOMAIN] [-p PORT] [-s SERVICE] [--json]   # No arg = *.up.railway.app
railway volume list|add [-m MOUNT_PATH]|delete [-v VOLUME] [-y]|update|detach|attach   # alias: volumes
railway bucket list|create [NAME] [-r REGION]|delete|info|credentials|rename   # alias: buckets
```

## Rules

- ALWAYS specify `-s SERVICE` and `-e ENV` when working outside a linked directory
- ALWAYS use `--json` for scripted/machine-readable output
- ALWAYS confirm before destructive actions (delete project, delete service, etc.)
- NEVER expose tokens in logs or commits
- For CI/CD, use `RAILWAY_TOKEN` env var with `railway up --ci`
- Use `railway run <CMD>` to execute local commands with Railway variables injected
