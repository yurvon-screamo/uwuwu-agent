# uwuwu-agent

Конфиги, агенты, плагины и скиллы opencode + исходники uwuwu-server.

Живые конфиги opencode на машине (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки на этот репозиторий.

## Содержимое

- `server/` — uwuwu-server: Rust-сервер контура — канбан-конвейер задач, MCP API, relay к локальной Ollama. Деплой на Railway.
- `agents/` — методички агентов конвейера (analyst, developer, plan-reviewer, code-quality-reviewer, tester, dreamer); `TASK-TEMPLATE.md` — канон тел задач.
- `skills/` — скиллы opencode: инженерные (rust, python, typescript, csharp, leptos, playwright), правила (чистый код, тесты, безопасность, логирование, производительность), офисные форматы, UI, интеграции (Railway, Bunny, Hugging Face, Stitch).
- `plugins/` — плагины opencode: `goal-mode`, `qlty-review`, `screen-review`, `sticky-retry`; общий TypeScript-код и тесты — в `lib/`.
- `commands/` — слэш-команды opencode.
- `opencodereview/` — правила машинного ревью.
- `opencode.json`, `cli.json` — конфиги opencode.
- `link.sh` — привязка конфигов симлинками (повторный запуск безопасен, бэкапит существующие конфиги).
- `AGENTS.md` — инструкции агентам.
- `Dockerfile` — relay-сервис Railway (rathole-туннель).

## Стек

Rust (axum, tokio) · TypeScript · Railway · GitHub

## Лицензия

MIT
