# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
opencodereview/ правила ревью
agents/        агенты: developer, dreamer, code-quality-reviewer (праймари — ревью-статусы фабрики)
plugins/       goal-mode (судья целей), sticky-retry, ocv-v2-adapter (+ disabled/)
skills/        31 скилл (см. ниже)
cli/           uwuwu-cli — memory-база, projects-docs, SOFA (бинарь: ~/.cargo/bin/uwuwu-cli)
factory/       uwuwu-factory — будильник разработки + маршрутизатор ревью
cron/          uwuwu-cron — демон кронов (доски «Кроны <Proj>» → сессии агентов)
dreamer/       uwuwu-dreamer — дример: пост-обработка сессий (idle → агент dreamer)
triage/        uwuwu-triage — входная точка задач: TG-бот → триаж → канбан uwuwu
lib/           общие модули демонов (single-instance лок)
cli-tools/     atlassian, research
link.sh        установка симлинками
opencode.json  главный конфиг опенкода (jsonc)
cli.json       TUI-конфиг
AGENTS.md      системные инструкции (глобальные)
```

## Скиллы (31)

- **Architect (1)**: architect-ui-ux
- **Engineers (6)**: csharp · leptos · playwright · python · rust · typescript
- **Workflows (4)**: flow-agents-md-writer · flow-audit · flow-code-documentation · flow-merge-review
- **Office (4)**: docx · pdf · pptx · xlsx
- **Rules (11)**: clean-code · git-commit · lib-usage · logging · performance · qlty · scalability · security · test-rule · text-writing · ui
- **Visual (1)**: visual-diagrams
- **Integrations (4)**: bunny-cli · hf-cli · stitch · use-railway

## Установка

```bash
git clone https://github.com/yurvon-screamo/uwuwu-agent.git
cd uwuwu-agent
./link.sh
```

`link.sh` бэкапит существующие конфиги (`*.pre-link-*`) и создаёт симлинки. Повторный запуск безопасен. После — перезапустить opencode.

## SiYuan-first 

Вся база знаний живёт в SiYuan: статьи опыта и проектные доки через `uwuwu-cli`,
прямой доступ к докам — MCP-шлюз (107 tools).

### Фабрика разработки (uwuwu-factory)

Issue-driven → **task-driven**: задачи = строки канбан-досок «Задачи <Proj>» (uwuwu, origa),
тела задач = доки с тегом Dev/Spike/Bug, диалог = нативные комменты.
Демон — **будильник + маршрутизатор ревью + lifecycle окружений** (systemd, тик 3 мин):
Design → окружение задачи `~/agents-env/<row-id>/{repo, scratch, logs}` (repo = git worktree
от origin/master, ветка `work/task-<row-id>`; env-переменная `UWUWU_FACTORY_ENV_ROOT`) →
OpenCode-сессия девелопера cwd=repo + короткий spawn; «Ревью плана»/«Ревью результата» →
спавн/будильник ревьювера (праймари-агент `code-quality-reviewer`, 1 сессия на задачу,
cwd = repo задачи); возврат статуса из ревью → wakeup девелопера; комменты юзера в
ревью-статусах → ревьюверу; новые комменты/правки тела юзера → wakeup сессии (мёртвая →
карантин → рестарт по комменту); Done (мерж-крон агента / Spike / руки) → гашение обеих
сессий + снос окружения (чистое → rm, грязное → attic без target/node_modules + коммент,
ветка -d после prune) + чистка state. Проекты вне env-механики (`env: false` в конфиге —
сейчас true-washing: cfg.local не git-репо) живут по-старому — агент сам делает ворктри.
Решение маршрутизации — чистая функция `routeTask` (factory-lib, покрыта тестами).
Вся логика пайпа (тело, типы, PR, самозаведённый крон проверки мержа) — у агента:
глобальный AGENTS.md «Работа над задачей» + AGENTS.md репо проекта.
`factory/` — Bun/TS: конфиг board/repo/local/env, env-lifecycle (factory/env.ts,
идемпотентный ensureEnv + cleanupEnv + старт-свип сирот), session-recovery MCP, state+reconcile.
Имена ревью-статусов — контракт с колонками досок: константы `REVIEW_PLAN_STATUS`/`REVIEW_RESULT_STATUS`.
Ячейки Path/«Сессия dev»/«Сессия ревью» — фабрика пишет best-effort (колонки заводит владелец).
Шаблоны тел задач (стандартный, spike) и правила формата — [factory/TASK-TEMPLATE.md](factory/TASK-TEMPLATE.md).

### Кроны (uwuwu-cron)

Кроны распределены по проектам: крон живёт на доске **«Кроны <Proj>»** в папке
проекта (создаётся руками: kanban-доска + колонки Schedule/Status/Last Run).
Строка = крон: Schedule (cron-expr | `5m/2h/1d` | разовое `09:30`), промпт =
тело-док строки. Демон (тик 1 мин) поллит все существующие крон-доски (нет
доски — тихо скип): наступило расписание → OpenCode-сессия с промптом из тела;
здоровый тик тихий — факт тика пишется демоном в Last Run, коммент агента
только исключение (падение/блокер/вопрос владельцу); разовые → авто-`fired`.
Перед спавном строка перепроверяется по свежему срезу доски (гонка с
`cron rm` снята). Глобальная доска «Crons» — легаси: лежит в трэше, не
используется.

CLI:
`uwuwu-cli cron set --title "…" [--project <proj>] (--in 10m | --every 30m | --at 09:30 | --cron '*/5 * * * *') [--stdin]`
(без `--project` целится в глобальную «Crons» — сейчас её нет, `set` упадёт с ошибкой),
`cron list / pause / resume / rm`.

### Триаж (uwuwu-triage)

Входная точка задач: TG-бот (@uwuwu_triage_bot) — заявка владельца текстом →
демон (long-poll) поднимает сессию триаж-агента (`glm-5.3-flash`, изолированный
workspace) → классификация (проект/тип) + дедуп → `task create` или коммент в
существующую → сводка в ТГ. Занят → персистентная очередь; заявка покидает
очередь только после подтверждённого промпта (рестарт не теряет); poison →
dead-letter с текстом заявки. Методичка агента — `triage/prompt.md` (в git,
правится без кода). systemd: `uwuwu-triage.service`. State:
`~/.local/state/uwuwu-triage/`, лог: `~/.local/share/opencode/log/uwuwu-triage.log`.

### Дример (uwuwu-dreamer)

Агентная пост-обработка сессий — единый контур накопления знаний (заменил
заявки `request` и qwen-фактизацию `facts`, оба механизма выпилены).
Демон на каждой ноде поллит `opencode.db` (readonly): уснувшая корневая
сессия (idle > 30 мин) → дочерняя сессия с агентом **dreamer**
(`glm-5.3#max`) → автономно: forensic-дамп сессии + git/PR-стейт проекта →
гигиена проектных доков (устаревшее/дыры), статьи опыта в Experience
(дедуп через серверный поиск), ADR (Accepted, сразу фактом), инфра-задачи на доске
uwuwu-проекта с пустым статусом (черновики владельца). **Инвариант:
1 рабочая сессия = 1 дример-сессия**: оживание исходной сессии будит того же
дримера wakeup-промптом (контекст прошлых записей сохраняется). Отчёт —
финальная строка. State: `~/.local/state/uwuwu-dreamer/`.

Запуск: линукс — systemd --user по образцу uwuwu-cron; вин —
`dreamer/win-autostart.ps1` (задача `\UWUWU\Dreamer`: старт при логоне +
повтор каждую минуту, живой инстанс отсекается IgnoreNew); ручной —
`cd dreamer && bun install && bun dreamer.ts`. Smoke:
`UWUWU_DREAMER_ONCE=1 UWUWU_DREAMER_DRY_RUN=1 bun dreamer.ts`.

### CLI

`uwuwu-cli task create/list/get/edit/comment/close [--project X]` — задачи и кроны
(id/заголовок; фоллбек — крон-доски проектов); `search` (сервер Manticore + семантика),
`read/blob/shot` (мультимодальное чтение), `person show/append/replace` — персона
владельца (агенты читают в начале работы, дример пишет факты после сессий).

Платформа — SiYuan (с 04.11.2026; до этого — закрытая платформа-предшественник). CLI ходит в
kernel SiYuan (REST + встроенный MCP), единый воркспейс на всех машинах.

**Production**: контейнер `siyuan-demo` на ноуте (`127.0.0.1:6806`), публично —
`https://siyuan-relay-production.up.railway.app` (Railway-релей + rathole-туннель)

Required env: `UWUWU_SIYUAN_URL`,
`UWUWU_SIYUAN_TOKEN` (или `UWUWU_SIYUAN_WS` с conf.json). Бекенд один — SiYuan kernel API
(сменимый бекенд выпилен PR #32, ADR-014).

**Workspace structure**:

- 📁 Experience — 33 subfolders by topic (cicd, rust, dotnet, llm-local, …), no direct docs
- 📁 Projects — 7 project subfolders, each starting with a `<Proj> Project (README)` doc (description carrier for `projects`)
- 📁 Daily — journal buckets (archive of the retired facts pipeline)

**Tags** = bare slugs for multi-PC addressing (`axum`, `origa/topology`).
Folders handle organization; tags are only for lookup.

**Commands**:

- `search` — полнотекст по блокам SiYuan (SQL kernel API); optional `--sofa`
- `read` — live doc read (slug resolves via migration map → tags/title through the gateway); rich content rendered inline (databases → tables, mindmaps → trees, canvas → shapes/connectors)
- `blob <key>` — download workspace blobs (images/attachments)
- `shot` — не портирован на SiYuan v1 (headless chromium + Access Auth Code при необходимости)
- `projects` — live workspace project registry
- `task` / `cron` / `body` — kanban tasks («Задачи <Proj>» boards, typed bodies), durable crons, task-body markdown (live diagrams: manual HTML block via SiYuan UI — ```html_embed fence conversion unimplemented on SiYuan v1, see `skills/visual-diagrams`)

**Infrastructure**: ноут (docker: siyuan-demo :6806, воркспейс ~/siyuan-demo-ws), наружу — Railway-релей + rathole (временно дефолт-домен Railway из-за бага сертов)

- Redis; Ollama relay (rathole tunnel from laptop); DNS on Bunny.
