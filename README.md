# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
opencodereview/ правила ревью
agents/        субагенты: code-quality-reviewer, developer, office-coworker
plugins/       goal-mode (судья целей), sticky-retry, ocv-v2-adapter (+ disabled/)
skills/        29 скиллов (см. ниже)
cli/           uwuwu-cli — memory-база, projects-docs, SOFA (бинарь: ~/.local/bin/uwuwu-cli)
cli-tools/     atlassian, research
link.sh        установка симлинками
opencode.json  главный конфиг опенкода (jsonc)
cli.json       TUI-конфиг
AGENTS.md      системные инструкции (глобальные)
```

## Скиллы (29)

- **Architect (1)**: architect-ui-ux
- **Engineers (6)**: csharp · leptos · playwright · python · rust · typescript
- **Workflows (5)**: flow-agents-md-writer · flow-audit · flow-code-documentation · flow-merge-review · flow-timesheet
- **Office (4)**: docx · pdf · pptx · xlsx
- **Rules (11)**: clean-code · git-commit · lib-usage · logging · performance · qlty · scalability · security · test-rule · text-writing · ui
- **Integrations (2)**: hf-cli · use-railway

## Установка

```bash
git clone https://github.com/yurvon-screamo/uwuwu-agent.git
cd uwuwu-agent
./link.sh
```

`link.sh` бэкапит существующие конфиги (`*.pre-link-*`) и создаёт симлинки. Повторный запуск безопасен. После — перезапустить opencode.

## AFFiNE-first 

Вся база знаний живёт в AFFiNE (Railway): articles/requests/facts через `uwuwu-cli`,
прямой доступ к докам — MCP-шлюз (107 tools).

### Фабрика разработки (uwuwu-factory)

Issue-driven → **task-driven**: задачи = строки канбан-досок «Задачи <Proj>» (uwuwu, origa),
тела задач = доки с тегом Dev/Spike/Bug, диалог = нативные комменты.
Демон — **только будильник** (systemd, тик 3 мин): Todo → worktree+OpenCode-сессия+короткий
spawn → In Progress; новые комменты/правки тела юзера → wakeup сессии (мёртвая → карантин
→ рестарт по комменту); Done (мерж-крон агента / Spike / руки) → чистка ворктри.
Вся логика пайпа (тело, типы, PR, самозаведённый крон проверки мержа) — у агента:
глобальный AGENTS.md «Работа над задачей» + AGENTS.md репо проекта.
`factory/` — Bun/TS: конфиг board/repo/local, session-recovery MCP, state+reconcile.

### Кроны (uwuwu-cron)

Доска «Crons» (папка в сайдбаре): Schedule (cron-expr | `5m/2h/1d` | разовое `09:30`),
промпт = тело-док. Демон (тик 1 мин) поднимает сессию агента по расписанию —
отчёты комментами 🤖 в тело; разовые → авто-`fired`.

CLI:
`uwuwu-cli cron set --title "…" (--in 10m | --every 30m | --at 09:30 | --cron '*/5 * * * *') [--stdin]`,
`cron list / pause / resume / rm`.

### CLI

`uwuwu-cli task create/list/get/edit/comment/close [--project X]` — задачи и кроны
(id/заголовок, фоллбек Crons); `request` (properties+attach), `facts`, `search`
(сервер Manticore + семантика), `read/blob/shot` (мультимодальное чтение).

The CLI is fully AFFiNE-backed — no filesystem knowledge base. All machines
share one workspace; locally only operational caches live.

**Production**: `https://affine.up.railway.app` (UI) / `https://affine-mcp-production.up.railway.app` (MCP gateway)

Required env: `UWUWU_AFFINE_URL`, `UWUWU_AFFINE_TOKEN`, `UWUWU_AFFINE_WORKSPACE_ID`.

**Workspace structure**:

- 📁 Experience — 33 subfolders by topic (cicd, rust, dotnet, llm-local, …), no direct docs
- 📁 Projects — 7 project subfolders, each starting with a `<Proj> Project (README)` doc (description carrier for `projects`)
- 📁 Daily — journal buckets (one per day, facts inside)
- 📁 Requests — knowledge article queue

**Tags** = bare slugs for multi-PC addressing (`axum`, `origa/topology`).
Folders handle organization; tags are only for lookup.

**Commands**:

- `search` — two-tier: AFFiNE server-side full-text (Manticore) first, automatic fallback to local semantic search (Ollama `qwen3-embedding:0.6b`)
- `read` — markdown + rich content rendered inline (databases → tables, mindmaps → trees, canvas → shapes/connectors)
- `blob <key>` — download workspace blobs (images/attachments)
- `shot <slug> [--edgeless]` — doc screenshot via headless chromium (the native render path — AFFiNE has no server-side PNG API)
- `grep` / `projects` — workspace content via title cache
- `request` — queue as AFFiNE docs (Requests folder, `Request` tag)
- `facts` — extraction → dedup (cross-PC content-hash) → mandatory daily bucket publish
- `refresh` — full cache refresh from AFFiNE (new machine entry point, ~800 docs)
- `task` / `cron` / `body` — kanban tasks («Задачи <Proj>» boards, typed bodies), durable crons, task-body markdown with ```html_embed diagram fences (Vesper theme, see `skills/visual-diagrams`)

**Infrastructure**: Railway (Singapore) — AFFiNE + MCP gateway + Postgres (pgvector)

- Redis; Ollama relay (rathole tunnel from laptop); DNS on Bunny.
