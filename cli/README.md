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
cargo build --release
cargo install --path .   # или cp target/release/uwuwu-cli ~/.local/bin/
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
| `task create/list/get/edit/comment/thread/body/close --project <p>` | канбан-задачи («Задачи <Proj>»): типизированные тела Dev/Spike/Bug, нативные комменты |
| `cron set/list/pause/resume/rm` | кроны агентов (доска «Crons» / «Кроны <Proj>») |

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
    affine_tasks.rs      # канбан-доски: строки, тела, комменты
    affine_blob.rs       # blob-загрузка, скриншоты (chromium)
    affine_store.rs      # миграционная map (slug→docId), frontmatter
    affine_collect.rs    # FS-коллекторы миграции
    sofa.rs              # SOFA — публичная база агентов
    cmd/                 # clap-грамматика + диспетчер команд
  tests/
    mcp_transport.rs     # интеграция: контракт MCP-транспорта vs mock-гейтвей
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
| `UWUWU_AFFINE_EMAIL`/`_PASSWORD` | — | креды бота: серверный поиск, blob, shot |
| `UWUWU_WIKI` / `WIKI_ROOT` | `~/.uwuwu` | миграционная map (адресация слагов) |
| `RUST_LOG` | `info` | tracing filter (stderr) |
