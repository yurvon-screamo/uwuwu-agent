# uwuwu → SiYuan миграция

Перенос воркспейса uwuwu (AFFiNE) на SiYuan: контент, структура, блобы, канбан-доски.

Источник истины по решению: задача-спайк «замена афины» (uwuwu). Владелец выбрал
SiYuan (визуал, database-канбан, таск = документ + саб-доки, плагины, нативный MCP).

## Пайплайн (все стадии resume-safe)

```
export (афа → ~/siyuan-migration-work/export)   # медленно: шлюз афины ~13с/док
  └─ manifest.json {docs, blobs, errors}
transform (export → transformed)                 # html_embed → raw HTML, blob refs
blobs (manifest.blobs → blobs/)                  # UI API sign-in + GET blobs
import (--assets → upload, map)                  # SiYuan Kernel API: notebooks + дерево
boards (--comments)                              # SiYuan MCP: database + kanban + комменты
```

## Запуск (полный цикл)

```bash
cd tools
python3 -m siyuan_migrate.export                  # ~1-2ч на 1068 доков (шлюз медленный)
python3 -m siyuan_migrate.transform
python3 -m siyuan_migrate.blobs                   # env: UWUWU_AFFINE_UI_URL/EMAIL/PASSWORD
python3 -m siyuan_migrate.import_siyuan --assets  # залить блобы, собрать _asset_map.json
python3 -m siyuan_migrate.import_siyuan           # залить доки (идемпотентно, скип по hPath)
python3 -m siyuan_migrate.boards --comments       # доски «Задачи/Кроны <Proj>» + комменты
```

Параметры целевого инстанса: `--ws` (путь к workspace, для api-токена), `--url`
(например http://127.0.0.1:6806). Инстанс-приёмник в демо-контуре: контейнер
`siyuan-demo` (systemd user-юнит `siyuan-demo.service`, quadlet —
`deploy/siyuan-demo/`), воркспейс `~/siyuan-demo-ws`.

## Трансформации контента

- ` ```html_embed имя ` фенсы → содержимое (raw HTML) инлайном: SiYuan рендерит
  HTML-блоки нативно.
- `affine://blob/<key>` → `assets/<key>` → при импорте рерайтится в реальный путь
  загруженного актива (`_asset_map.json`).
- Имена файлов = реальные заголовки доков (из export payload), коллизии → ` (2)`.

## Доски

Доска афины = док с database-блоком. Миграция: `read_database_cells` → строки
(Title/Status/Branch/PR/linkedDocId) → SiYuan database (Status=select, Branch/PR=text)
+ строки + kanban-вид (группировка по Status). Комменты задач (треды+реплаи)
аппендятся секцией `## Комменты (миграция из афины)` в тело задачи (поиск по заголовку).

## Честные потери (v1)

- истории правок и ревизии афины;
- реакции и нативные треды (комменты становятся секцией в теле);
- edgeless-майндмапы/канва (использовались редко);
- live-связь строка↔тело: строки database независимы, тела — отдельные доки
  (навигация по заголовку);
- саб-ноутбуки афинных баз данных внутри обычных доков (экспортируются GFM-таблицами).

## Отчётность

- `export/manifest.json` — статусы всех доков/блобов/ошибок;
- `boards_report.json` — что создано по доскам.
