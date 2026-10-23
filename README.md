# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
agents/        субагенты: code-quality-reviewer, developer, office-coworker
plugins/       goal-mode (судья целей), sticky-retry, ocv-v2-adapter (+ disabled/)
skills/        29 скиллов (см. ниже)
cli/           uwuwu-cli — memory-база, projects-docs, SOFA (бинарь: ~/.local/bin/uwuwu-cli)
cli-tools/     atlassian, research
opencodereview/ правила ревью
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

## Маркетинговый зоопарк

Живёт отдельно: `~/origa/marketing/.opencode` (репа [origa](https://github.com/yurvon-screamo/origa)) — 6 агентов, TG-хаб, humanizer-конвейер.

## Сборка uwuwu-cli

```bash
cd cli && cargo build --release
```

## License

MIT

## AFFiNE-first (2026-10)

The CLI is fully AFFiNE-backed — no filesystem knowledge base. All machines
share one workspace; locally only operational caches live (`affine_content_cache.json`,
`affine_embedding_cache.json`, `affine_facts_index.json`, `affine_map_*.json`).

Required env: `UWUWU_AFFINE_URL`, `UWUWU_AFFINE_TOKEN`, `UWUWU_AFFINE_WORKSPACE_ID`.

- `search` — semantic search over the workspace (local Ollama embeddings)
- `read` / `grep` / `projects` — workspace content (slug tags address docs)
- `request` — queue as AFFiNE docs ("Requests" container)
- `facts` — extraction → dedup → mandatory publish into daily buckets
- `write` — publish an article/project doc straight from stdin
- `sync` — full cache resync (new machine entry point)
- `affine --all` / `--tag-slugs` — one-time legacy migration / tag backfill
