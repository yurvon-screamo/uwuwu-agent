# uwuwu-cli

CLI для платформы uwuwu (**hub-бекенд**: приватное репо `yurvon-screamo/uwuwu-hub` —
issues = задачи, Projects v2 = канбаны, docs md = вики). Rust + clap; транспорт —
`gh` (REST + GraphQL), вики — через ленивый git-клон uwuwu-hub
(`~/.local/share/uwuwu-cli/hub-clone/<owner>__<repo>`). **Stateless**: контент-кэша
нет — каждая команда читает живьём; hub недоступен → честная ошибка.

Накопление знаний (статьи опыта, правки доков, инфра-задачи) делает
**дример** — агентная пост-обработка сессий (`dreamer/` в корне репо).
CLI заявку/фактизацию/кэш не содержит.

## Setup

```bash
cd cli
cargo install --path .
```

**Канон бинаря — `~/.cargo/bin/uwuwu-cli` (дефолтный `$CARGO_HOME/bin`), копия ровно одна**; установка/обновление — только `cargo install --path .`. Не копируй артефакт вручную в `~/.local/bin`: он в PATH стоит раньше `~/.cargo/bin` и молча затенит свежий бинарь — обновления перестанут применяться. Смок после установки:

```bash
which -a uwuwu-cli  # ровно одна строка: ~/.cargo/bin/uwuwu-cli
uwuwu-cli --help    # грамматика актуальна
uwuwu-cli task list --project uwuwu  # живой ответ доски
```

## Контракт ответов мутаций

Каждая мутирующая команда (task create/edit/claim/comment/close/body,
person append/replace) возвращает **либо явную ошибку, либо финальное состояние
затронутой сущности** (read-after-write), не эхо ввода. Отказ — ненулевой exit
+ текст на stderr с подсказкой следующего хода; частично применённая мутация
называет, что применилось и как докатить.

## Usage

| Команда | Что делает |
| --- | --- |
| `projects` | имена топ-папок вики (Experience/Projects/Daily); перечень проектов — `docs ls "Projects"` |
| `read <slug>` | чтение вики-дока: слаг → docs/<slug>.md (фоллбек basename-поиск по docs/**); markdown |
| `docs ls "<project>[/<folder>]"` | перечень доков папки вики |
| `person show\|append\|replace` | персона владельца (вики-док Experience/person/PERSON): show — markdown (агенты читают в начале работы); append — дописать долговременный факт (append_doc сам читает+конкатенирует); replace — полная перезапись. Мутации возвращают финальный документ или явную ошибку |
| `search "…" [--limit N --threshold X]` | универсальный: полнотекст по docs/** клона + доки по заголовкам + семантика локального эмбеддинг-индекса (Ollama); title/snippet/UI-ссылка, top-n на метод |
| `semantic rebuild\|status\|sync` | обслуживание локального эмбеддинг-индекса (Ollama `qwen3-embedding:0.6b`, SQLite) |
| `task create\|list\|get\|edit\|comment\|thread\|body\|close\|rm\|clean\|undo --project <p>` | канбан-задачи («Задачи <Proj>» = Projects v2): issue + item канбана; статусы — Status-поле Pv2 (канон Draft…Вейт), клетки — глобальные issue fields (Приоритет/Autodone/Branch/PR/Сессии/Расписание/Запланировано; ADR-098, fm-шапки тел снесены — тело = чистый контент); новая задача — **Draft** (триаж валидирует → Todo), `--status Todo` — машинные создания; rm — только Draft; clean — батч-снос с guard'ом и undo-журналом |
| `cron list [--project <p>]` | мастера кронов (задачи с расписанием, статус **Вейт**); крон заводится `task create/edit --schedule "…"` |

вызов даёт громкий отказ с подсказкой hub-аналогов.

## Architecture

```
cli/
  Cargo.toml
  src/
    lib.rs               # модули + run()
    main.rs              # thin binary entry
    store/
      github_task*.rs    # TaskStore: issues + Projects v2 (gh REST/GraphQL)
      github_doc_*.rs    # DocStore: md-доки через git-клон (ff-only push)
      github_gh.rs       # gh-транспорт (таймауты, envelope-барьеры)
      task_store.rs      # TaskBoard/CreateTask/EditTask — контракт трейтов
    cmd/
      hub_task.rs        # task-семья (GitHubStore)
      hub_docs.rs        # docs/read (GitHubDocStore)
      run.rs             # маршрутизация + hub_repo_default()
  tests/
    path_guard_e2e.rs    # гварды входов (--md/--attach/--base) на уровне процессов
    deploy_artifacts.rs  # артефакты деплоя (journald-лимиты)
    github_store_e2e.rs  # живой e2e GitHubStore (UWUWU_E2E_GITHUB=1, #[ignore])
```

## How search works

Универсальный поиск (три секции): полнотекст по docs/** git-клона,
доки по заголовкам (basename/hpath) и семантика локального эмбеддинг-индекса
(Ollama `qwen3-embedding:0.6b`, пер-машинный SQLite
`~/.local/share/uwuwu-cli/semantic.db`). Выдача — top-n на метод: title,
snippet, ссылка. Отказы маршрута (Ollama лежит, индекс пуст) — честный
статус в секции, команду не роняют.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `UWUWU_HUB_REPO` | `yurvon-screamo/uwuwu-hub` | hub-репо (пустое значение = дефолт) |
| `GH_TOKEN` | — | токен GitHub (классический PAT с repo; иначе gh ходит в keyring) |
| `UWUWU_HUB_CLONE_ROOT` | `~/.local/share/uwuwu-cli/hub-clone` | база клонов вики |
| `RUST_LOG` | `info` | tracing filter (stderr) |
