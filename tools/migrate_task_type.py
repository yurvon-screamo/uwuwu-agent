#!/usr/bin/env python3
"""Одноразовая миграция: тип задачи (bug/dev/spike) из тега тела → колонку «Тип» доски.

До миграции cli писал тип тегом (#dev/#bug/#spike) отдельным абзацем в конце
тела; после (PR task-type) тип живёт в select-колонке «Тип» доски задач,
теги в телах не пишутся. Скрипт докатывает легаси: для каждой строки досок
«Задачи *» находит тег-абзац в теле, записывает клетку «Тип» и сносит тег.

Запуск (после мержа cli, пишущего колонку):
  python3 tools/migrate_task_type.py --dry-run   # только отчёт
  python3 tools/migrate_task_type.py             # миграция + верификация

Клетка пишется через database item_update (ядро минтит опцию select —
прецедент Status), тег сносится block delete (только лист-абзац тега).
Неканонические случаи (тег не отдельным абзацем, несколько тегов) — в отчёт,
строка не трогается.

Идемпотентность: клетка заполнена + тега нет → no-op; заполненная клетка с
живым тегом → клетка не перезаписывается, тег сносится.
Бэкап: render-снапшот + av-файлы + экспорты тел с тегами в
<home>/siyuan-backups/av-task-type-<ts>/ (--backup-dir переопределяет).

Откат: остановить контейнер siyuan-demo → восстановить data/storage/av/*.json
из каталога бэкапа (docker exec/docker cp: файлы воркспейса принадлежат
контейнерному uid) → старт контейнера. Тела восстанавливаются вручную из
body-*.md экспортов бэкапа (тег вернулся — клетка остаётся, некритично).
"""
import argparse
import json
import os
import re
import shutil
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

WS = Path.home() / "siyuan-demo-ws"
PAGE_SIZE = 50
SY_ID = re.compile(r"\d{14}-[a-z0-9]+")
TAGS = {"#bug": "bug", "#dev": "dev", "#spike": "spike"}
TYPE_KEYS = ("Тип", "Type")
# абзац-дамп get_children: `- [p] текст (block-id)`; lute экранирует ведущий #
TAG_PARA = re.compile(r"^- \[p\] (.+?) \((\d{14}-[a-z0-9]+)\)\s*$")

_ID = [0]
_SID = None


def _token() -> str:
    conf = json.loads((WS / "conf/conf.json").read_text())
    return conf["api"]["token"]


def _base() -> str:
    return os.environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")


def _workspace() -> Path:
    return Path(os.environ.get("UWUWU_SIYUAN_WS", str(WS)))


def _post_json(path: str, body: dict) -> dict:
    url = f"{_base()}{path}"
    if not url.startswith(("http://", "https://")):
        raise ValueError(f"неожиданная схема запроса: {url}")
    req = urllib.request.Request(
        url,
        json.dumps(body).encode(),
        {"Authorization": f"Token {_token()}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:  # nosec B310 — схема проверена выше, хост из env
        payload = json.load(resp)
    if payload.get("code") != 0:
        raise RuntimeError(f"{path}: {payload}")
    return payload.get("data") or {}


def _rpc(method: str, params: dict | None = None) -> dict:
    result = _rpc_full(method, params)
    if "structuredContent" in result:
        return result["structuredContent"]
    raw = (result.get("content") or [{}])[0].get("text", "{}")
    return json.loads(raw)


def _rpc_full(method: str, params: dict | None = None) -> dict:
    global _SID
    body: dict = {"jsonrpc": "2.0", "method": method, "params": params or {}}
    if not method.startswith("notifications/"):
        _ID[0] += 1
        body = {"jsonrpc": "2.0", "id": _ID[0], "method": method, "params": params or {}}
    headers = {
        "Authorization": f"Token {_token()}",
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    if _SID:
        headers["Mcp-Session-Id"] = _SID
    req = urllib.request.Request(f"{_base()}/mcp", json.dumps(body).encode(), headers)
    with urllib.request.urlopen(req, timeout=60) as resp:  # nosec B310 — _base() из env, только локальный kernel
        sid = resp.headers.get("Mcp-Session-Id")
        if sid:
            _SID = sid
        if method.startswith("notifications/"):
            return {}
        text = resp.read().decode()
    data_lines = [line[6:] for line in text.splitlines() if line.startswith("data: ")]
    payload = json.loads("\n".join(data_lines) if data_lines else text)
    if "error" in payload:
        raise RuntimeError(f"mcp {method}: {payload['error']}")
    result = payload.get("result", {})
    if result.get("isError"):
        raise RuntimeError(f"mcp {method}: {json.dumps(result.get('content'))[:200]}")
    return result


def _sql(stmt: str) -> list[dict]:
    return _post_json("/api/query/sql", {"stmt": stmt}) or []


def _db(action: str, **kw) -> dict:
    return _rpc("tools/call", {"name": "database", "arguments": {"action": action, **kw}})


def _block(action: str, **kw) -> str:
    """block-инструмент отдаёт сырой текст (дамп get_children), не JSON."""
    result = _rpc_full("tools/call", {"name": "block", "arguments": {"action": action, **kw}})
    return (result.get("content") or [{}])[0].get("text", "")


def _sql_by_id(column: str, block_id: str) -> str:
    """Инвариант для SQL-интерполяции: id только в формате kernel [0-9]{14}-[a-z0-9]+."""
    if not SY_ID.fullmatch(block_id):
        raise ValueError(f"подозрительный id: {block_id!r}")
    stmt = f"select {column} from blocks where id='{block_id}' limit 1"  # nosec B608 — SQL API ядра не принимает параметры; id проверен fullmatch выше, column — литерал из кода
    return stmt


def av_id_from_markdown(markdown: str) -> str | None:
    match = re.search(r'data-av-id="(\d{14}-[a-z0-9]+)"', markdown) or re.search(
        r'avID="(\d{14}-[a-z0-9]+)"', markdown
    )
    return match.group(1) if match else None


def find_av_blocks() -> list[dict]:
    """Все database-блоки воркспейса: av-блок ядра + legacy i-носитель."""
    seen: set[str] = set()
    out: list[dict] = []
    queries = (
        "select id, markdown, root_id from blocks where type='av' and markdown like '%av-id=%' limit 500",
        "select id, markdown, root_id from blocks where type='i' and markdown like '%avID=%' limit 500",
    )
    for stmt in queries:
        for block in _sql(stmt):
            av_id = av_id_from_markdown(block.get("markdown") or "")
            if not av_id or av_id in seen:
                continue
            seen.add(av_id)
            out.append({"av_id": av_id, "block_id": block["id"]})
    return out


def board_docs() -> dict[str, str]:
    docs = _sql("select id, hpath from blocks where type='d' limit 3000")
    return {d["id"]: d.get("hpath", "?") for d in docs}


def type_key_of(av_id: str) -> dict | None:
    keys = (_db("keys", id=av_id).get("data") or {}).get("keys") or []
    return next((key for key in keys if key.get("name") in TYPE_KEYS), None)


def mint_type_key(av_id: str) -> str:
    """Создать select-колонку «Тип»: keyID генерит клиент (формат node-id)."""
    stamp = datetime.now().strftime("%Y%m%d%H%M%S")
    tail = "".join(os.urandom(7).hex()[i] for i in range(7))  # nosec B311 — id ядра, не секрет
    key_id = f"{stamp}-{tail}"
    _post_json(
        "/api/av/addAttributeViewKey",
        {"avID": av_id, "keyID": key_id, "keyName": "Тип", "keyType": "select",
         "keyIcon": "", "previousKeyID": ""},
    )
    return key_id


def raw_rows_of(view: dict) -> list[dict]:
    """Строки/карточки страницы: table несёт rows, kanban — groups[].cards."""
    rows = list(view.get("rows") or [])
    for group in view.get("groups") or []:
        rows.extend(group.get("cards") or [])
    return rows


def row_cells(row: dict, key_names: dict) -> tuple[str, str, str]:
    """(item_id, body_doc_id, тип-значение) строки render."""
    body_doc = ""
    task_type = ""
    row_id = ""
    for cell in row.get("cells") or []:
        value = cell.get("value") or {}
        name = key_names.get(value.get("keyID"))
        if value.get("type") == "block":
            row_id = row.get("id") or value.get("blockID", "")
            body_doc = (value.get("block") or {}).get("id", "")
        elif name in TYPE_KEYS:
            options = value.get("mSelect") or []
            task_type = options[0].get("content", "") if options else ""
    return row_id, body_doc, task_type


def render_page(av_id: str, block_id: str, page: int) -> tuple[list[tuple[str, str, str]], dict]:
    """(item_id, body_doc_id, тип-значение) одной страницы render + view."""
    data = _db("render", id=av_id, blockID=block_id, page=page, pageSize=PAGE_SIZE).get("data") or {}
    view = data.get("view") or {}
    key_names = {c.get("id"): c.get("name") for c in view.get("columns") or []}
    rows = [
        triple
        for row in raw_rows_of(view)
        for triple in [row_cells(row, key_names)]
        if triple[0]
    ]
    return rows, view


def render_all(av_id: str, block_id: str) -> list[tuple[str, str, str]]:
    rows: list[tuple[str, str, str]] = []
    seen: set[str] = set()
    page = 1
    while True:
        page_rows, view = render_page(av_id, block_id, page)
        for item in page_rows:
            if item[0] not in seen:
                seen.add(item[0])
                rows.append(item)
        total = view.get("rowCount") or view.get("cardCount") or len(rows)
        if page * PAGE_SIZE >= total or not page_rows:
            return rows
        page += 1


def tag_blocks_of(body_doc: str) -> tuple[list[str], str | None]:
    """Теги тела: (block_id, канон). Все найденные теги одного канона
    (механический дубль записи) — легитимно: сносим все, канон один.
    Разные каноны — реальный конфликт: (MULTIPLE-id, первый канон) как
    аномалия, строка не трогается."""
    found: list[tuple[str, str]] = []
    for line in _block("get_children", id=body_doc).splitlines():
        match = TAG_PARA.match(line)
        if not match:
            continue
        canon = TAGS.get(match.group(1).strip().lstrip("\\"))
        if canon:
            found.append((match.group(2), canon))
    if not found:
        return [], None
    canons = {canon for _, canon in found}
    if len(canons) > 1:
        return ["MULTIPLE"], found[0][1]
    return [block_id for block_id, _ in found], found[0][1]


def snapshot(av_id: str, block_id: str) -> list[dict]:
    cells = []
    for row_id, body_doc, task_type in render_all(av_id, block_id):
        cells.append({"row": row_id, "body": body_doc, "type": task_type})
    return cells


def collect_boards() -> list[dict]:
    """Доски «Задачи *» (hpath корня = /<proj>/Задачи <proj>) + снапшот строк."""
    docs = board_docs()
    boards: list[dict] = []
    for block in find_av_blocks():
        root_rows = _sql(_sql_by_id("root_id", block["block_id"]))
        root = root_rows[0]["root_id"] if root_rows else ""
        hpath = docs.get(root, "?")
        # доска задач: последний сегмент hpath «Задачи <proj>» («Цели …» мимо)
        if not hpath.rsplit("/", 1)[-1].startswith("Задачи "):
            continue
        boards.append({
            "av_id": block["av_id"],
            "block_id": block["block_id"],
            "doc": hpath,
            "key": type_key_of(block["av_id"]),
            "cells": snapshot(block["av_id"], block["block_id"]),
        })
    return boards


def backup(boards: list[dict], backup_dir: Path) -> None:
    """render-снапшот + av-файлы + экспорты тел с тегами (undo-база)."""
    backup_dir.mkdir(parents=True, exist_ok=True)
    av_dir = _workspace() / "data/storage/av"
    for board in boards:
        av_id = board["av_id"]
        (backup_dir / f"render-{av_id}.json").write_text(
            json.dumps({"cells": board["cells"], "key": board["key"]}, ensure_ascii=False, indent=1)
        )
        src = av_dir / f"{av_id}.json"
        if src.exists():
            shutil.copy2(src, backup_dir / f"{av_id}.json")
    tagged = [
        (cell, board["doc"])
        for board in boards
        for cell in board["cells"]
        if cell.get("tag")
    ]
    for cell, doc in tagged:
        body = cell.get("body") or ""
        if not body or not SY_ID.fullmatch(body):
            continue
        export = _post_json("/api/export/exportMdContent", {"id": body})
        content = (export or {}).get("content") or ""
        safe = re.sub(r"[^A-Za-z0-9а-яА-ЯёЁ _-]", "_", f"{doc}__{cell['row']}")[:120]
        (backup_dir / f"body-{safe}.md").write_text(content, encoding="utf-8")


def dry_run(boards: list[dict], backup_dir: Path) -> None:
    backup_dir.mkdir(parents=True, exist_ok=True)
    report = {b["av_id"]: b["cells"] for b in boards}
    (backup_dir / "dryrun-report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=1)
    )
    for b in boards:
        tagged = sum(1 for c in b["cells"] if c.get("tag"))
        weird = sum(1 for c in b["cells"] if c.get("tag") == ["MULTIPLE"])
        suffix = f", с тегом {tagged}" if tagged else ""
        suffix += f", аномальных {weird}" if weird else ""
        key = "есть" if b["key"] else "НЕТ (будет создана)"
        print(f"  {b['doc']} [{key}] строк {len(b['cells'])}{suffix}")
    print("dry-run: снапшот в", backup_dir)


def migrate_cell(board: dict, av_id: str, key_id: str, cell: dict, stats: list[int]) -> None:
    """Одна строка: клетка из тега + снос тегов; stats = [written, removed, already, weird]."""
    if cell.get("tag") == ["MULTIPLE"]:
        print(f"  ⚠ {board['doc']}: {cell['row']} — теги разных типов, не трогаю", file=sys.stderr)
        stats[3] += 1
        return
    canon = cell.get("tag_type")
    if not cell["type"] and canon:
        _db("item_update", id=av_id, keyID=key_id, itemID=cell["row"],
            value={"mSelect": [{"content": canon}]})
        stats[0] += 1
        time.sleep(0.3)
    elif cell["type"]:
        stats[2] += 1
    for tag_id in cell.get("tag") or []:
        if SY_ID.fullmatch(tag_id):
            _post_json("/api/block/deleteBlock", {"id": tag_id})
            stats[1] += 1
            time.sleep(0.3)


def migrate(boards: list[dict]) -> tuple[int, int, int, int]:
    """Клетка из тега + снос тегов. Возвращает (клеток записано, тегов снесено,
    уже заполнено, аномальных)."""
    stats = [0, 0, 0, 0]
    for board in boards:
        av_id = board["av_id"]
        key = board["key"]
        key_id = key["id"] if key else mint_type_key(av_id)
        for cell in board["cells"]:
            migrate_cell(board, av_id, key_id, cell, stats)
    return tuple(stats)


def verify_cell(board: dict, after: dict, cell: dict, mismatches: list[str]) -> None:
    """Одна строка: клетка несёт тип, теги снесены."""
    expected = cell["type"] or cell.get("tag_type")
    if cell.get("tag") == ["MULTIPLE"]:
        return
    now = after.get(cell["row"])
    if expected and now != expected:
        mismatches.append(
            f"{board['doc']}: {cell['row']} клетка={now!r} (ожидался {expected!r})"
        )
    if cell.get("tag") and tag_blocks_of(cell["body"])[0]:
        mismatches.append(
            f"{board['doc']}: {cell['row']} тег не снесён ({cell.get('tag')})"
        )


def verify(boards: list[dict]) -> list[str]:
    """Повторный render + get_children: клетка несёт тип, теги снесены."""
    mismatches: list[str] = []
    for board in boards:
        key = type_key_of(board["av_id"])
        if board["key"] is None and key is None:
            mismatches.append(f"{board['doc']}: колонка «Тип» не создалась")
            continue
        after = {
            row_id: task_type
            for row_id, _, task_type in render_all(board["av_id"], board["block_id"])
        }
        for cell in board["cells"]:
            verify_cell(board, after, cell, mismatches)
    return mismatches


def tag_boards(boards: list[dict]) -> None:
    """Добавить в снапшот теги тел: (block_ids, канон) каждой строки."""
    for board in boards:
        for cell in board["cells"]:
            if not cell["body"] or not SY_ID.fullmatch(cell["body"]):
                cell["tag"], cell["tag_type"] = None, None
                continue
            tag, canon = tag_blocks_of(cell["body"])
            cell["tag"], cell["tag_type"] = tag, canon


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="только отчёт, без изменений")
    parser.add_argument("--backup-dir", default=None, help="каталог бэкапа (деф. ~/siyuan-backups/av-task-type-<ts>)")
    parser.add_argument("--only", metavar="AV_ID", help="мигрировать только эту database (smoke/точечный прогон)")
    args = parser.parse_args()

    boards = collect_boards()
    tag_boards(boards)
    if args.only:
        boards = [b for b in boards if b["av_id"] == args.only]
        if not boards:
            print(f"database {args.only} не найдена или не доска задач", file=sys.stderr)
            return 2
    tagged = sum(1 for b in boards for c in b["cells"] if c.get("tag"))
    weird = sum(1 for b in boards for c in b["cells"] if c.get("tag") == ["MULTIPLE"])
    print(f"досок задач: {len(boards)}, строк с тегом: {tagged} (аномальных {weird})")

    if args.dry_run:
        backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / "siyuan-backups/av-task-type-dryrun"
        dry_run(boards, backup_dir)
        return 0

    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / f"siyuan-backups/av-task-type-{ts}"
    backup(boards, backup_dir)
    print("бэкап:", backup_dir)

    written, removed, already, weird = migrate(boards)
    print(f"клеток записано: {written}, тегов снесено: {removed}, уже заполнено: {already}, аномальных: {weird}")

    mismatches = verify(boards)
    if mismatches:
        print("РАСХОЖДЕНИЯ:", file=sys.stderr)
        for line in mismatches:
            print(" ", line, file=sys.stderr)
        return 1
    print("верификация: клетки несут тип, теги снесены")
    return 0


if __name__ == "__main__":
    try:
        _rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "migrate-task-type", "version": "1.0"}})
        _rpc("notifications/initialized")
        sys.exit(main())
    except urllib.error.URLError as error:
        print(f"SiYuan недоступен: {error}", file=sys.stderr)
        sys.exit(2)
