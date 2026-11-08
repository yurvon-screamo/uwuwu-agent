# uwuwu-agent

Конфиги, агенты, плагины и скиллы опенкода + исходники uwuwu-cli. Репозиторий — единственное место, где файлы существуют: живые конфиги (`~/.config/opencode`, `~/.agents`, `~/.claude/skills`) — симлинки сюда.

## Структура

```
opencodereview/ правила ревью
agents/        агенты: developer, dreamer, code-quality-reviewer (праймари — ревью-статусы фабрики)
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

## SiYuan-first 

Вся база знаний живёт в SiYuan: статьи опыта и проектные доки через `uwuwu-cli`
(kernel SiYuan — REST + встроенный MCP; `uwuwu-cli read/docs ls/...`).

### Фабрика разработки (uwuwu-factory)

Issue-driven → **task-driven**: задачи = строки канбан-досок «Задачи <Proj>» (uwuwu, origa),
тела задач = доки с тегом Dev/Spike/Bug, диалог = комменты в теле задачи (секция «Комменты»).
Демон — **будильник + маршрутизатор ревью + lifecycle окружений** (systemd, тик 3 мин):
Design → окружение задачи `~/agents-env/<row-id>/{repo, scratch, logs}` (repo = git worktree
от origin/master, ветка `work/task-<row-id>`; env-переменная `UWUWU_FACTORY_ENV_ROOT`) →
OpenCode-сессия девелопера cwd=repo + короткий spawn; «Ревью плана»/«Ревью результата» →
спавн/будильник ревьювера (праймари-агент `code-quality-reviewer`, 1 сессия на задачу,
cwd = repo задачи); возврат статуса из ревью → wakeup девелопера; перевод владельцем
трека в In Progress — будильник/рестарт девелопера (конфликтный PR даёт конфликт-kickoff);
комменты юзера в ревью-статусах → ревьюверу; новые комменты/правки тела юзера → wakeup
сессии (мёртвая → карантин → рестарт по комменту); Done (мерж-крон агента / Spike / руки)
→ гашение обеих сессий + снос окружения (чистое → rm, грязное → attic без
target/node_modules + коммент, ветка -d после prune) + чистка state. Проекты вне
env-механики (`env: false` в конфиге — сейчас true-washing: cfg.local не git-репо) живут
по-старому — агент сам делает ворктри.
Решение маршрутизации — чистая функция `routeTask` (factory-lib, покрыта тестами).
Вся логика пайпа (тело, типы, PR, самозаведённый крон проверки мержа) — у агента:
глобальный AGENTS.md «Работа над задачей» + AGENTS.md репо проекта.
`factory/` — Bun/TS: конфиг board/repo/local/env, env-lifecycle (factory/env.ts,
идемпотентный ensureEnv + cleanupEnv + старт-свип сирот), session-recovery MCP, state+reconcile.
Имена ревью-статусов — контракт с колонками досок: константы `REVIEW_PLAN_STATUS`/`REVIEW_RESULT_STATUS`.
Ячейки Path/«Сессия dev»/«Сессия ревью» — фабрика пишет best-effort (колонки заводит владелец); агент заявляет свою сессию на строку командой `task claim` (колонка «Сессия dev» на доске обязательна — без неё claim громко отказывает).
Канон секций, шаблоны тел задач (dev, spike, bug) и правила формата — [factory/TASK-TEMPLATE.md](factory/TASK-TEMPLATE.md).

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

- `search` — universal: полнотекст по блокам SiYuan (SQL kernel API) + доки по заголовкам + семантика локального эмбеддинг-индекса; `--limit/--threshold/--sofa`
- `read` — live doc read (slug → docId через kernel API, markdown-экспорт ядра); rich content rendered inline
- `blob <key>` — download workspace blobs (images/attachments)
- `shot` — не портирован на SiYuan v1 (headless chromium + Access Auth Code при необходимости)
- `projects` — live workspace project registry
- `task` / `cron` / `body` — kanban tasks («Задачи <Proj>» boards, typed bodies), кроны = задачи с расписанием (`--schedule`, мастера в **Вейт**) + `cron list`, task-body markdown (live diagrams: ```html_embed fences convert to SiYuan html-blocks, marker-addressed `⁂ diagram:<name>`, see `skills/visual-diagrams`)

**Infrastructure**: ноут (docker: siyuan-demo :6806, воркспейс ~/siyuan-demo-ws), наружу — Railway-релей + rathole (временно дефолт-домен Railway из-за бага сертов)

- DNS on Bunny (siyuan.uwuwu.net за Pull Zone — ждёт фикс сертов Railway); Ollama — локальная на ноуте (semantic-индекс cli, localhost-only; релей rathole-ollama погашен 05.11). Redis афин-стека не живёт (афина остановлена).
