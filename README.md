# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
opencodereview/ правила ревью
agents/        агенты: developer, dreamer, plan-reviewer, code-quality-reviewer, tester; TASK-TEMPLATE.md — канон тел задач
plugins/       goal-mode (судья целей), sticky-retry, ocv-v2-adapter (+ disabled/)
skills/        скиллы (см. ниже)
cli/           uwuwu-cli — задачи/кроны, поиск, вики-чтение, персона (бинарь: ~/.cargo/bin/uwuwu-cli)
lib/           общие модули плагинов опенкода (qlty-review, screen-review, goal-lib)
link.sh        установка симлинками
opencode.json  главный конфиг опенкода (jsonc)
cli.json       TUI-конфиг
AGENTS.md      системные инструкции (глобальные)
```

> 14.11.2026: демон-контур (factory/cron/triage/dreamer, cli-tools, tools,
> scripts, deploy, TS-тесты) снесён — перепись на единый Rust-сервер
> uwuwu-server (задача hub#1133). Бэкап снесённого — ветка
> `backup/pre-server-rewrite`.

## Скиллы (30)

- **UI (2)**: architect-ui-ux (дизайн-стратегия + правила имплементации; бывшая пара architect/rules-ui слита) · stageshow (faux-UI макеты, анимации, bento — vendored, MIT, upstream benjitaylor/skills @ 299140f)
- **Engineers (6)**: csharp · leptos · playwright · python · rust · typescript
- **Workflows (3)**: flow-agents-md-writer · flow-audit · flow-code-documentation
- **Office (4)**: docx · pdf · pptx · xlsx
- **Rules (10)**: clean-code · git-commit · lib-usage · logging · performance · qlty · scalability · security · test-rule · text-writing
- **Visual (1)**: visual-diagrams
- **Integrations (4)**: bunny-cli · hf-cli · stitch · use-railway

## Установка

```bash
git clone https://github.com/yurvon-screamo/uwuwu-agent.git
cd uwuwu-agent
./link.sh
```

`link.sh` бэкапит существующие конфиги (`*.pre-link-*`) и создаёт симлинки. Повторный запуск безопасен. После — перезапустить opencode.

## Hub-first

Вся база знаний живёт в приватном репо **uwuwu-hub**: статьи опыта и проектные
доки — docs/*.md, задачи — issues + Projects v2 канбаны (`uwuwu-cli
read/docs ls/task/...`).

### Конвейер uwuwu (остановлен 14.11.2026 — перепись на uwuwu-server)

Локальные демоны (factory/cron/triage/dreamer) остановлены и снесены из репо —
контур переписывается в единый Rust-сервер `uwuwu-server` (Railway, MCP API):
постановка и зафигсированные решения — задача hub#1133 (Design Spec), бэкап
снесённого кода — ветка `backup/pre-server-rewrite`. Кроны меняют носитель на
вики-папку `docs/Projects/<proj>/Кроны/` (решение по hub#1058).

Контракты, которые переживают перепись (канон поведения нового конвейера):

- **Статусная модель и переходы** — глобальный AGENTS.md «Работа над задачей»
  (Draft → Todo → Design → Ревью плана → In Progress → Ревью результата →
  Тестирование → Review → Done; Needs attention, Вейт).
- **Автодон** (клетка «Autodone», дефолт `task create` — on с 14.11): вход в
  Review → сервер мержит PR и ставит Done; красный CI/конфликт — обратно в
  In Progress; без PR — Done сразу.
- **Очередь CUP** (кап активных LLM-задач, деф. 7, глобально; приоритеты
  high → medium → low) и **пик-окно GLM** (будни 06:00–10:00 UTC, off-выключатель):
  спавны/будильники не-high откладываются до конца окна.
- **Крон-грамматика**: cron-expr | `5m/2h/1d` | разовое `09:30 [01.12]`;
  мастер в Вейте, файр только из Вейт, ран-карточка Todo, Done рана ставит
  агент рана сам; остановка мастера — Done навсегда.
- **Claim**: агент заявляет свою сессию в ячейку «Сессия dev» (`task claim`) —
  конвейер уважает живую заявку, дубль не спавнит. Сессионные ячейки
  (dev/ревью/тест) — привязка стадий к сессиям опенкода.
- **Канон тел задач** — [agents/TASK-TEMPLATE.md](agents/TASK-TEMPLATE.md).

### CI (GitHub Actions)

`.github/workflows/ci.yml` — на каждый PR: cargo fmt --check, cargo clippy
--all-targets -- -D warnings, cargo test (e2e скипаются без живого ядра) и
qlty delta-check по конфигу репо. Зелёный CI — обязательное условие мержа
(branch protection master, required check «CI»); ревьювер ручных прогонов
не делает — опирается на CI-статусы. Локально то же самое: `cd cli && cargo
fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

### Кроны (uwuwu-cron — контур снесён, см. «Конвейер uwuwu»)

Кроны унифицированы с задачами: крон = задача на доске «Задачи <Proj>» с
расписанием (`--schedule`), мастер в Вейт, ран-карточка «⏰ …» в Todo, инвентарь
`cron list`. Носитель меняется на вики-папку (hub#1058) в новом сервере; канон
статусов крон-семьи и грамматика — в секции «Конвейер uwuwu» выше.

### Триаж (uwuwu-triage — контур снесён, см. «Конвейер uwuwu»)

Входная точка задач (TG-бот → триаж-агент → Draft/коммент) переезжает в
uwuwu-server; ТГ-часть — hub#315. Канон валидации черновиков — глобальный
AGENTS.md.

### Дример (uwuwu-dreamer — контур снесён, см. «Конвейер uwuwu»)

Пост-обработка сессий (инвариант 1 рабочая сессия = 1 дример-сессия) переезжает
в uwuwu-server на REST-only чтение сессий (без opencode.db). Методичка агента —
`agents/dreamer.md`.

### CLI

`uwuwu-cli task create/list/get/edit/comment/thread/body/close/rm [--project X]` — задачи
(id/заголовок; id-команды без `--project` — глобальный резолв по доскам всех проектов
с требованием уникальности: матчи на нескольких досках/неоднозначность — громкая ошибка;
доска «Задачи <proj>», крон-доски сняты; кроны — `--schedule` у task, мастера в Вейт,
инвентарь `cron list`); аттачи в тело — `--attach` на `create`/`body`
(kernel `/api/asset/upload`, kernel-имена, референсы append-ом); переименование —
`edit --title` (renameDoc + verify Title-клетки, `/` в тайтле → `∕`); `search`
(полнотекст + доки по заголовкам + семантика локального эмбеддинг-индекса),
`read/blob` (мультимодальное чтение; `shot` не портирован), `person show/append/replace` —
персона владельца (агенты читают в начале работы, дример пишет факты после сессий).

Платформа — **uwuwu-hub** (с 13.11.2026; история носителей: закрытый предшественник → легаси-бекенд → hub). CLI ходит в GitHub (gh REST/GraphQL), вики — git-клон uwuwu-hub, один инстанс на все машины.

**Production**: приватное репо `yurvon-screamo/uwuwu-hub` (GitHub); локальный
git-клон вики — `~/.local/share/uwuwu-cli/hub-clone/yurvon-screamo__uwuwu-hub`.

Required env: `UWUWU_HUB_REPO` (дефолт зашит в cli), `GH_TOKEN` (gh; иначе keyring).

**Workspace structure**:

- 📁 Experience — 33 subfolders by topic (cicd, rust, dotnet, llm-local, …), no direct docs
- 📁 Projects — 7 project subfolders, each starting with a `<Proj> Project (README)` doc (description carrier for `projects`)
- 📁 Daily — journal buckets (archive of the retired facts pipeline)

**Tags** = bare slugs for multi-PC addressing (`axum`, `origa/topology`).
Folders handle organization; tags are only for lookup.

**Commands**:

- `search` — universal: полнотекст по docs/** клона + доки по заголовкам + семантика локального эмбеддинг-индекса; `--limit/--threshold`
- `read` — live doc read (слаг → docs/<slug>.md клона); markdown
- `projects` — имена топ-папок вики (Experience/Projects/Daily)
- `task` / `cron` / `body` — kanban tasks («Задачи <Proj>» = Projects v2, типизированные тела), кроны = задачи с расписанием (`--schedule`, мастера в **Вейт**) + `cron list`; диаграммы в телах — mermaid (GitHub рендерит нативно), see `skills/visual-diagrams`

**Infrastructure**: GitHub (приватное репо uwuwu-hub) + локальный git-клон вики; gh-токен в env (`GH_TOKEN`).
