# uwuwu-cli

CLI для базы знаний uwuwu (AFFiNE-backed): живое чтение доков, серверный
поиск, канбан-задачи, кроны. Rust + clap + MCP-шлюз афины. **Stateless**:
локального контент-кэша нет — каждая команда ходит в афину живьём, стейл-
чтения невозможны по построению (гейт недоступен → честная ошибка).

Накопление знаний (статьи опыта, правки доков, инфра-задачи) делает
**дример** — агентная пост-обработка сессий (`dreamer/` в корне репо).
CLI заявку/фактизацию/кэш не содержит (механизмы выпилены).

## Setup

```bash
cd cli
cargo install --path .
```

**Канон бинаря — `~/.cargo/bin/uwuwu-cli` (дефолтный `$CARGO_HOME/bin`), копия ровно одна**; установка/обновление — только `cargo install --path .`. Не копируй артефакт вручную в `~/.local/bin`: он в PATH стоит раньше `~/.cargo/bin` и молча затенит свежий бинарь — обновления перестанут применяться. Смок после установки:

```bash
which -a uwuwu-cli  # ровно одна строка: ~/.cargo/bin/uwuwu-cli
uwuwu-cli --help    # грамматика актуальна
```

Локальная Ollama больше не нужна (семантический фоллбек выпилен вместе с
контент-кэшем; поиск — серверный Manticore афины).

## Usage

| Команда | Что делает |
| --- | --- |
| `projects` | реестр проектов (живой проход по организ-дереву, описания из README-доков) |
| `read <slug>` | живое чтение: слаг → миграционная map → теги/тайтл через гейт; rich-контент (базы/майндмапы/канва) рендерится в текст |
| `blob <key> [--out f]` | скачать blob (ключи видны в read как `affine://blob/<key>`) |
| `shot <slug> [--edgeless]` | скриншот дока через headless chromium |
| `search "…" [--limit N --sofa N]` | серверный полнотекстовый (Manticore): title/snippet/UI-ссылка с docId; SOFA опционально |
| `task create/list/get/edit/comment/thread/body/close/rm --project <p>` | канбан-задачи («Задачи <Proj>»): типизированные тела Dev/Spike/Bug, нативные треды комментов — ответ в тред: `comment <id> --reply <префикс>`; create идемпотентен — ретрай после таймаута достраивает сироту доски, дубль тайтла = явная ошибка; новая строка всегда в статусе **Draft** («задачи в пустоту не заводим» — триаж валидирует и переводит в Todo); rm снимает строку и трэшит тело (сироту — по docId), **только Draft-строки** — живую задачу перед удалением переведи в Draft (`task edit <id> --status Draft`) |
| `cron set/list/pause/resume/rm` | кроны агентов (доска «Crons» / «Кроны <Proj>»); set идемпотентен как task create, rm трэшит сироту без строки |

## Architecture

```
cli/
  Cargo.toml
  src/
    lib.rs               # модули + run()
    main.rs              # thin binary entry
    affine_client.rs     # MCP-шлюз: JSON-RPC over SSE, re-handshake, retry
    affine_queries.rs    # read-сторона: живой резолв слагов, read, rich-рендер, projects
    affine_docs.rs       # markdown-чтение, резолв слагов (map → tags → title)
    affine_search_server.rs # серверный полнотекстовый поиск (Manticore) — единственный
    affine_children.rs   # резолв существующего тела: строки+дети доски → сирота/дубль/чисто
    affine_comments.rs   # треды комментов: парсинг replies, snapshot, резолв префиксов
    affine_gql.rs        # прямой GraphQL афины: createReply (гейтвей reply не экспонирует)
    affine_tasks.rs      # канбан-доски: строки, тела, комменты; идемпотентный create (task+cron), rm
    affine_blob.rs       # blob-загрузка, скриншоты (chromium)
    affine_store.rs      # миграционная map (slug→docId), frontmatter
    affine_collect.rs    # FS-коллекторы миграции
    sofa.rs              # SOFA — публичная база агентов
    cmd/                 # clap-грамматика + диспетчер команд
  tests/
    common/mod.rs        # mock-шлюз (сценарные шаги) + изоляция HOME
    mcp_transport.rs     # интеграция: контракт MCP-транспорта vs mock-гейтвей
    task_create_flow.rs  # интеграция: идемпотентность create, rm — против mock-гейтвея
```

## How search works

Серверный полнотекстовый поиск афины (Manticore): свежий индекс всего
воркспейса, env `UWUWU_AFFINE_UI_URL`/`_EMAIL`/`_PASSWORD`. Выдача:
title, snippet, UI-ссылка с docId. Локального корпуса/семантики нет —
stateless by design.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `UWUWU_AFFINE_URL` | — | MCP-шлюз AFFiNE (обязательно) |
| `UWUWU_AFFINE_TOKEN` | — | токен бота (обязательно) |
| `UWUWU_AFFINE_WORKSPACE_ID` | — | id воркспейса (обязательно) |
| `UWUWU_AFFINE_UI_URL` | `https://affine.uwuwu.net` | UI-хост: ссылки + серверный поиск |
| `UWUWU_AFFINE_EMAIL`/`_PASSWORD` | — | креды бота: серверный поиск, blob, shot, ответы в треды |
| `UWUWU_WIKI` / `WIKI_ROOT` | `~/.uwuwu` | миграционная map (адресация слагов) |
| `RUST_LOG` | `info` | tracing filter (stderr) |
