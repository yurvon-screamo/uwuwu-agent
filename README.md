# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
opencodereview/ правила ревью
agents/        агенты: developer, dreamer, plan-reviewer (праймари «Ревью плана»), code-quality-reviewer (праймари «Ревью результата»)
plugins/       goal-mode (судья целей), sticky-retry, ocv-v2-adapter (+ disabled/)
skills/        скиллы (см. ниже)
cli/           uwuwu-cli — задачи/кроны, поиск, вики-чтение, персона (бинарь: ~/.cargo/bin/uwuwu-cli)
factory/       uwuwu-factory — будильник разработки + маршрутизатор ревью
cron/          uwuwu-cron — планировщик крон-задач (доски «Задачи <Proj>» → ран-карточки → сессии)
dreamer/       uwuwu-dreamer — дример: пост-обработка сессий (idle → агент dreamer)
triage/        uwuwu-triage — входная точка задач: TG-бот → триаж → канбан uwuwu
lib/           общие модули демонов (single-instance лок)
cli-tools/     atlassian, research
link.sh        установка симлинками
opencode.json  главный конфиг опенкода (jsonc)
cli.json       TUI-конфиг
AGENTS.md      системные инструкции (глобальные)
```

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

### Фабрика разработки (uwuwu-factory)

Issue-driven → **task-driven**: задачи = строки канбан-досок «Задачи <Proj>» (uwuwu, origa),
тела задач = доки с тегом Dev/Spike/Bug, диалог = комменты в теле задачи (секция «Комменты»).
Демон — **будильник + маршрутизатор ревью и тестирования + lifecycle окружений** (systemd, тик 3 мин):
Design → окружение задачи `~/agents-env/<row-id>/{repo, scratch, logs}` (repo = git worktree
от origin/master, ветка `work/task-<row-id>`; env-переменная `UWUWU_FACTORY_ENV_ROOT`) →
OpenCode-сессия девелопера cwd=repo + короткий spawn; «Ревью плана» → спавн/будильник
план-ревьювера (праймари-агент `plan-reviewer`, своя сессия на стадию, cwd = repo задачи);
«Ревью результата» → спавн/будильник ревьювера результата (праймари-агент
`code-quality-reviewer`, своя сессия на стадию, cwd = repo задачи); «Тестирование» →
спавн/будильник тестировщика (праймари-агент
`tester`, тоже 1 сессия на задачу); возврат статуса из ревью/тестирования → wakeup
девелопера; перевод владельцем
трека в In Progress — будильник/рестарт девелопера (конфликтный PR даёт конфликт-kickoff);
комменты юзера в ревью-статусах → ревьюверу; новые комменты/правки тела юзера → wakeup
сессии (мёртвая → карантин → рестарт по комменту); Done (мерж-крон агента / Spike / руки)
→ гашение обеих сессий + снос окружения (чистое → rm, грязное → attic без
target/node_modules + коммент, ветка -d после prune) + чистка state. Проекты вне
env-механики (`env: false` в конфиге — сейчас true-washing: cfg.local не git-репо) живут
по-старому — агент сам делает ворктри.
Решение маршрутизации — чистая функция `routeTask` (factory-lib, покрыта тестами).
Гард dead-спавна (задача 1072): dead-вердикт serve-пробы (`sessionLiveness`, 404/400)
сверяется с независимым следом opencode.db — `session_v2.time_updated` сессии
(id-ключ) или свежайшая сессия каталога `env/repo` (directory-ключ при легитимно
пустой дев-клетке, инстанс #315); fresh → спавн отложен (serve при рестартах/
деградации лжёт 404 при живом ходе — инстанс 13.11, задача 317), stale/unknown →
прежнее поведение. Окно `UWUWU_FACTORY_DEAD_FRESH_MIN` (деф. 30 мин, 0 = гард
выключен), путь БД `UWUWU_FACTORY_OPENCODE_DB` (деф. `~/.local/share/opencode/opencode.db`).
Вся логика пайпа (тело, типы, PR, самозаведённый крон проверки мержа) — у агента:
глобальный AGENTS.md «Работа над задачей» + AGENTS.md репо проекта.
Автодон/очередь/пик (задача 20261112160316): галочка «Autodone» (ставит только владелец) —
фабрика сама мержит PR на входе в Review (GitHub gh / Gitea tea+API, merge-commit) и ставит
Done; красный CI/конфликт — обратно в In Progress; без PR — Done сразу. Очередь CUP
(`UWUWU_FACTORY_CUP`, деф. 7 глобально) — новые Design/In Progress берутся по приоритету
(high → medium → low, колонка «Приоритет»; create без флага пишет medium —
дефолт владельца 13.11, триаж явно ставит high/low). Пик-окно GLM
(`UWUWU_FACTORY_PEAK`, деф. будни 06:00–10:00 UTC = 15:00–19:00 JST, off-выключатель) —
спавны/будильники не-high откладываются до конца окна.
`factory/` — Bun/TS: конфиг board/repo/local/env, env-lifecycle (factory/env.ts,
идемпотентный ensureEnv + cleanupEnv + старт-свип сирот), session-recovery MCP, state+reconcile,
autodone (factory/autodone.ts — носитель PR, вердикты проб, DRY_RUN-гейт merge-вызовов).
Имена ревью-статусов и стадии тестирования — контракт с колонками досок: константы
`REVIEW_PLAN_STATUS`/`REVIEW_RESULT_STATUS`/`TESTING_STATUS` (factory-lib).
Ячейки Path/«Сессия dev»/«Сессия ревью»/«Сессия тест» — фабрика пишет best-effort (колонки заводит владелец); агент заявляет свою сессию на строку командой `task claim` (колонка «Сессия dev» на доске обязательна — без неё claim громко отказывает). Спавн фабрики на доске без сессионной колонки тоже громкий: коммент в задачу с расхождением (ADR-054), раз на доску — тихая потеря привязки рождает дубль-агентов.
Канон секций, шаблоны тел задач (dev, spike, bug — docs/other делят dev-костяк, analys делит spike) и правила формата — [factory/TASK-TEMPLATE.md](factory/TASK-TEMPLATE.md).

### CI (GitHub Actions)

`.github/workflows/ci.yml` — на каждый PR: cargo fmt --check, cargo clippy
--all-targets -- -D warnings, cargo test (e2e скипаются без живого ядра) и
qlty delta-check по конфигу репо. Зелёный CI — обязательное условие мержа
(branch protection master, required check «CI»); ревьювер ручных прогонов
не делает — опирается на CI-статусы. Локально то же самое: `cd cli && cargo
fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

### Кроны (uwuwu-cron)

Кроны унифицированы с задачами — выделенный контур снесён. Крон = обычная
задача на доске **«Задачи <Proj>»** с расписанием в доке-теле: атрибут
`custom-schedule` (грамматика: cron-expr | `5m/2h/1d` | разовое `09:30 [01.12]`),
ставится `task create/edit --schedule "…"` с валидацией при записи; та же
команда синхронно пишет колонку **«Расписание»** на доске (отображение attr —
хранилище остаётся в доке-теле). Дата запуска — свойство строки:
**«Запланировано»** (date-колонка, сортировка/фильтр/группировка штатно).
У ран-карточки — фактическое время файра (`task edit --planned`; тайтловая
метка остаётся только для уникальности тайтла), у мастера — вычисленный
next-fire: демон сверяет клетки каждым тиком (compare по epoch ms —
formattedContent ядро перекрывает своей локалью) и дописывает при
расхождении; интервал/разовое отвечают сразу, у свежего cron-мастера до
первого файра клетка пуста (файр на первом совпадении — reconcile заполняет
после, вечный молчун годовым сканом тик не таскает). Мастер живёт в статусе **Вейт** = «ждёт файра планировщика»
(файр-гейт демона читает именно его; после валидации триажем — грамматика +
разумность; Draft не файрится). Статус-модель крон-семьи: **Todo** (ран) =
«запуск состоялся, работа выполняется агентом», **Done** = тихий успех
(агент рана закрывает свой ран сам в финале прогона — ран запись запуска,
не задача, приёмки успешного тика не требуется), **Needs attention** —
блокер/вопрос владельцу. Планировщик
(тик 1 мин) по расписанию создаёт **ран-карточку** — новую задачу
«⏰ <мастер> · <дата время>» в Todo с датой файра — и OpenCode-сессию с
промптом из тела мастера (изолированный workspace); запись запуска: владелец
видит ЧТО и КОГДА происходило; ран, оставшийся в Todo после тика, — сигнал
сбоя (сессия не дошла до финала), разбор владельцем.
Коммент агента — только исключение (на ран; блокер/вопрос → Needs attention
на ране). Перед спавном мастер перепроверяется по свежему срезу доски
(гонка со снятием строки); файр — только из Вейт, Done = остановлено
навсегда. Разовый мастер после файра снимает расписание одной CLI-командой
(`task edit --schedule "" --planned ""` — attr и обе клетки). State:
`~/.local/state/uwuwu-cron/` (пер-машинный), CLI: `cron list [--project
<proj>]` — мастера + last run (вне прод-машины last run пуст). Доски «Кроны
<Proj>», их колонки и CLI set/pause/resume/rm сняты (задача
20261106182051-o5ejlte).

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
(`glm-5.3-flash#high`) → автономно: forensic-дамп сессии + git/PR-стейт проекта →
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

`uwuwu-cli task create/list/get/edit/comment/thread/body/close/rm [--project X]` — задачи
(id/заголовок; id-команды без `--project` — глобальный резолв по доскам всех проектов
с требованием уникальности: матчи на нескольких досках/неоднозначность — громкая ошибка;
доска «Задачи <proj>», крон-доски сняты; кроны — `--schedule` у task, мастера в Вейт,
инвентарь `cron list`); аттачи — файл руками в `assets/` репо uwuwu-hub,
raw-ссылка в тело (cli-флаг `--attach` на `create`/`body` на hub громко
отказывает); переименование —
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
