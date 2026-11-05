# uwuwu-cli

CLI для базы знаний uwuwu (SiYuan-бекенд): живое чтение доков, полнотекстовый
поиск, kernel-пробы, канбан-задачи, кроны. Rust + clap, SiYuan kernel API
(REST + встроенный MCP-клиент database). **Stateless**: локального контент-
кэша нет — каждая команда ходит в SiYuan kernel живьём, стейл-чтения
невозможны по построению (kernel недоступен → честная ошибка).

**Дисциплина волн деградации**: мутационные вызовы на «metadata is not ready»
ретраятся ≤3 раз (+2s/+5s), затем bail с хинтом (`uwuwu-cli doctor`) и 90s
внутрипроцессным fail-fast окном — волна кормится ретраями, таранить её
бесполезно и вредно (репро 04.11: серии с шагом ~2–3с = ретраи клиента).
Прочие ошибки ретраются как прежде и окно не армят.

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
контент-кэшем; поиск — серверный SQL like по блокам SiYuan).

## Usage

| Команда | Что делает |
| --- | --- |
| `projects` | реестр проектов (живой проход по организ-дереву, описания из README-доков) |
| `read <slug>` | живое чтение: слаг → docId (hpath/тайтл через kernel API); markdown-экспорт дока |
| `blob <key> [--out f]` | скачать ассет из assets/ воркспейса SiYuan |
| `kernel ping\|sql\|file\|ls\|export\|render [--timeout SECS]` | живые kernel-пробы из агентской сессии без env-токена (токен из конфигурации cli): one-shot без ретраев — сырое состояние ядра на первом ответе; вердикты (Bytes/Missing, пустой export/ls) → exit 0, неудача пробы (Failed/таймаут/код≠0/auth 401) → exit ≠0. Классы: живость+auth (`ping`), деградация индекса («metadata is not ready») и произвольная диагностика (`sql`), «tree not found»/FS-истина (`file`/`ls`), пустые экспорты (`export`), доски/пагинация строк (`render`: view.rowCount/view.rows as-is) |
| `doctor [--project uwuwu] [--since MIN]` | диагностика деградации ядра (read-only): kernel live, wave-скан docker logs («commit tx failed»; DEGRADED при ≥5 за окно, меньше — шум), доска проекта (мёртвые привязки, пустые ZWJ-тела), FS↔SQL count по открытым ноутбукам. Exit 0/1/2: чисто / деградация (включая недоступное ядро) / пробы не выполнились. Контейнер — `UWUWU_SIYUAN_CONTAINER` (деф. siyuan-demo), FS-проба — локальный `UWUWU_SIYUAN_WS` |
| `shot <slug> [--edgeless]` | скриншот дока через headless chromium |
| `search "…" [--limit N --sofa N]` | серверный полнотекстовый (SQL like по блокам SiYuan): title/snippet/UI-ссылка с docId; SOFA опционально |
| `task create/list/get/edit/comment/thread/body/close/rm --project <p>` | канбан-задачи («Задачи <Proj>»): типизированные тела Dev/Spike/Bug, нативные треды комментов — ответ в тред: `comment <id> --reply <префикс>`; create идемпотентен — ретрай после таймаута достраивает сироту доски, дубль тайтла = явная ошибка; новая строка всегда в статусе **Draft** («задачи в пустоту не заводим» — триаж валидирует и переводит в Todo); rm снимает строку и трэшит тело (сироту — по docId), **только Draft-строки** — живую задачу перед удалением переведи в Draft (`task edit <id> --status Draft`) |
| `cron set/list/pause/resume/rm` | кроны агентов (доска «Кроны <Proj>», деф. uwuwu; `--project` у всей семьи); set требует колонки Schedule/Status/Last Run — нет колонки → громкий отказ до мутаций (без сирот); строка привязана к телу (промпт), Status — mSelect, дубль-тайтл — отказ; list печатает SCHEDULE и LAST RUN; rm трэшит сироту без строки |

## Architecture

```
cli/
  Cargo.toml
  src/
    lib.rs               # модули + run()
    main.rs              # thin binary entry
| файл | роль |
| --- | --- |
| `siyuan.rs` | клиент SiYuan kernel (REST) + встроенный MCP (database) |
| `siyuan_task.rs` | таск-модель: доски/строки/тела/комменты |
| `cmd/sy_dispatch.rs` | хендлеры всех команд (единственный бекенд) |
    cmd/                 # clap-грамматика + диспетчер команд
  tests/
    common/mod.rs        # mock-шлюз (сценарные шаги) + изоляция HOME
    mcp_transport.rs     # интеграция: контракт MCP-транспорта vs mock-гейтвей
    task_create_flow.rs  # интеграция: идемпотентность create, rm — против mock-гейтвея
```

## How search works

Серверный полнотекстовый поиск SiYuan (SQL like по блокам kernel API):
свежий индекс всего — title, snippet, UI-ссылка с docId. Локального
корпуса/семантики нет — stateless by design.

## Env vars

| Variable | Default | Description |
| --- | --- | --- |
| `UWUWU_SIYUAN_URL` | `http://127.0.0.1:6806` | kernel SiYuan (REST+MCP) |
| `UWUWU_WIKI` / `WIKI_ROOT` | `~/.uwuwu` | миграционная map (адресация слагов) |
| `RUST_LOG` | `info` | tracing filter (stderr) |
