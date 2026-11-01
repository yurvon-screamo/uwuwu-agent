# uwuwu-cli

CLI для базы знаний uwuwu (AFFiNE-backed): чтение и поиск по воркспейсу,
канбан-задачи, кроны. Rust + clap + MCP-шлюз афины + локальные кэши
(embeddings — Ollama). Единственный источник истины — AFFiNE-воркспейс;
локально живут только операционные кэши.

Накопление знаний (статьи опыта, правки доков, инфра-задачи) делает
**дример** — агентная пост-обработка сессий (`dreamer/` в корне репо).
CLI заявку/фактизацию не содержит (механизмы выпилены).

## Setup

```bash
cd cli
cargo build --release
cargo install --path .   # или cp target/release/uwuwu-cli ~/.local/bin/
```

Требуется ollama с моделью семантического поиска:

```bash
ollama pull qwen3-embedding:0.6b
```

## Usage

| Команда | Что делает |
| --- | --- |
| `projects` | список проектов воркспейса |
| `read <slug>` | статья опыта (`axum`), док проекта (`<proj>/<slug>`), SOFA (`sofa/<id>`); rich-контент рендерится в текст |
| `blob <key> [--out f]` | скачать blob (ключи видны в read как `affine://blob/<key>`) |
| `shot <slug> [--edgeless]` | скриншот дока через headless chromium |
| `search "…" [--top N --tasks N --access N --sofa N]` | серверный полнотекстовый (Manticore) + фоллбек на локальную семантику |
| `grep "подстрока"` | substring-поиск по контент-кэшу (статьи + доки проектов) |
| `task create/list/get/edit/comment/thread/body/close --project <p>` | канбан-задачи («Задачи <Proj>»): типизированные тела Dev/Spike/Bug, нативные комменты |
| `cron set/list/pause/resume/rm` | кроны агентов (доска «Crons» / «Кроны <Proj>») |
| `refresh` | полный пересинк локальных кэшей (новая машина, после массовых правок) |

## Architecture

```
cli/
  Cargo.toml
  src/
    lib.rs               # модули + run()
    main.rs              # thin binary entry
    affine_client.rs     # MCP-шлюз: JSON-RPC over SSE, re-handshake, retry
    affine_queries.rs    # read-сторона: session/sync кэша, read, grep, rich-рендер
    affine_docs.rs       # markdown-чтение, резолв слагов
    affine_search.rs     # локальная семантика: корпус, квоты по kind, косинус
    affine_search_server.rs # серверный полнотекстовый поиск (Manticore)
    affine_tasks.rs      # канбан-доски: строки, тела, комменты
    affine_blob.rs       # blob-загрузка, скриншоты (chromium)
    affine_store.rs      # миграционная map (slug→docId), frontmatter
    affine_collect.rs    # FS-коллекторы миграции
    content_cache.rs     # контент-кэш воркспейса (TTL, tree-blind деградация)
    embed.rs             # ollama embeddings
    cmd/                 # clap-грамматика + диспетчер команд
  tests/
    mcp_transport.rs     # интеграция: контракт MCP-транспорта vs mock-гейтвей
```

## How search works

1. Серверный полнотекстовый (Manticore) — свежий индекс, без локальных
   зависимостей; основной путь.
2. Недоступен/пусто → локальная семантика: статьи и доки проектов из
   контент-кэша чанкуются по параграфам, эмбеддятся `qwen3-embedding:0.6b`
   (кэш по content-hash, бюджет на прогон), косинус + квоты по kind
   (experience/access/tasks), тела задач/кронов — live по тегам с TTL-кэшем.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `UWUWU_AFFINE_URL` | — | MCP-шлюз AFFiNE (обязательно) |
| `UWUWU_AFFINE_TOKEN` | — | токен бота (обязательно) |
| `UWUWU_AFFINE_WORKSPACE_ID` | — | id воркспейса (обязательно) |
| `UWUWU_AFFINE_UI_URL` | `https://affine.uwuwu.net` | UI-хост для ссылок |
| `UWUWU_WIKI` / `WIKI_ROOT` | `~/.uwuwu` | корень локальных кэшей |
| `RUST_LOG` | `info` | tracing filter (stderr) |
