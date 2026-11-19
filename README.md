# uwuwu-agent

Конфиги, агенты, плагины и скиллы opencode + исходники uwuwu-server. Репозиторий — единственный носитель файлов: живые конфиги `~/.config/opencode`, `~/.agents`, `~/.claude/skills` — симлинки сюда.

Лицензия — MIT. Канбан и база знаний — в приватном репо [uwuwu-hub](https://github.com/yurvon-screamo/uwuwu-hub); правила работы конвейера — [AGENTS.md](AGENTS.md).

## Структура

| Путь | Назначение |
| ---- | ---------- |
| `agents/` | Методички агентов конвейера: developer, plan-reviewer, code-quality-reviewer, tester, analyst, analyst-reviewer, dreamer. `TASK-TEMPLATE.md` — канон тел задач. |
| `skills/` | Скиллы opencode: инженерные (rust, python, typescript, csharp, leptos, playwright), правила (чистый код, коммиты, тесты, безопасность, логирование, производительность, масштабируемость, qlty, проза), офисные форматы (docx, pdf, pptx, xlsx), UI-дизайн и faux-UI, визуализация, интеграции (Railway, Bunny, Hugging Face, Stitch). |
| `plugins/` | Плагины opencode: `goal-mode` (судья целей), `qlty-review`, `screen-review`, `sticky-retry`. |
| `lib/` | Общие TS-модули плагинов и их тесты (bun test). |
| `commands/` | Слэш-команды opencode. |
| `opencodereview/` | Правила машинного ревью кода. |
| `server/` | **uwuwu-server** — единый сервер контура: канбан-конвейер, MCP API, relay к локальной Ollama. Rust-workspace: крейты `domain`, `usecase`, `server`. |
| `server/ops/` | Операционные конфиги и скрипты: локальные конфиги памяти, примеры rathole, стенд. |
| `opencode.json` | Главный конфиг opencode (JSONC). |
| `cli.json` | Конфиг TUI opencode. |
| `link.sh` | Идемпотентная привязка конфигов симлинками. |
| `Dockerfile` | Relay-сервис Railway: туннель rathole между локальной Ollama и облаком. |
| `AGENTS.md` | Системные инструкции агентов — глобальный контекст каждой сессии. |

## Установка

```bash
git clone https://github.com/yurvon-screamo/uwuwu-agent.git
cd uwuwu-agent
./link.sh
```

Скрипт бэкапит существующие не-симлинки (`*.pre-link-*`) и создаёт симлинки. Повторный запуск безопасен. После установки перезапустите opencode.

## uwuwu-server

Единый сервер контура (перепись с локальных демонов, hub#1133): ведёт канбан-конвейер — статусные переходы, спавны агентов, автомерж, ревью-цепочки; раздаёт MCP-инструменты (`task_*`, `docs_*`, `search`, `person_show`, `goal_list`, `blob_*`); держит relay-туннель к локальной Ollama.

Прод — Railway. Мерж в `master` с изменениями `server/**` деплоится автоматически через GitHub-интеграцию (watch `server/**`). Health-check — `GET /health`.

Разработка:

```bash
cd server
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Конфигурация — `server/config.toml`; образец с комментариями — `server/config.example.toml`.

## CI

`.github/workflows/ci.yml` — параллельные джобы (fmt / clippy+test / plugins / qlty) с path-фильтрами: правка доков не ждёт rust-компиляцию. Агрегатор «CI» — required check branch protection.

Авто-триггеры сейчас отключены: биллинг GitHub Actions исчерпан (#1238/#1239), запуск — только вручную. Поэтому перед мержем гоняйте проверки локально. Активный автодеплой сервера — Railway GitHub-интеграция, отдельная deploy-джоба снята намеренно (двойной деплой).

## Hub-first

База знаний живёт в uwuwu-hub: `docs/` — статьи опыта и проектные доки, issues + Projects v2 — канбаны задач. Локальных клонов hub нет — агенты читают и пишут через MCP uwuwu-server.

Ключевые доки конвейера: статусная модель, каналы (тело vs комменты), PR-механика и кроны — [AGENTS.md](AGENTS.md); канон тел задач — [agents/TASK-TEMPLATE.md](agents/TASK-TEMPLATE.md).
