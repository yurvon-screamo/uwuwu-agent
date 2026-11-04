#!/usr/bin/env python3
"""Одноразовая миграция: ключ PR канбан-досок SiYuan text → url + перезапись значений.

Зачем два шага: kernel `key_update` меняет тип ключа, но НЕ мигрирует значения —
после конвертации `url.content` пуст, а старый текст остаётся в стейл-поле
`text.content` (наблюдалось на «Задачи origa»). Поэтому: key_update → item_update
url.content = старый text.content для каждой непустой ячейки.

Запуск (после мержа cli, пишущего url-формат):
  python3 tools/migrate_pr_url.py --dry-run   # только отчёт
  python3 tools/migrate_pr_url.py             # миграция + верификация

Идемпотентность: повторный запуск — no-op (url-ключи с заполненными url не трогаются).
Бэкап: <home>/siyuan-backups/av-pr-url-<ts>/ (--backup-dir переопределяет); каталог
воркспейса в запись недоступен (владелец файлов — контейнерный uid).

Откат: остановить контейнер siyuan-demo → восстановить data/storage/av/*.json из
каталога бэкапа (через docker exec/docker cp: файлы воркспейса принадлежат
контейнерному uid) → старт контейнера. Живое ядро держит av в памяти и подмену
файла может молча перезаписать.
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
    # SSE допускает разбиение события на несколько data-строк — соединяем
    payload = json.loads("\n".join(data_lines) if data_lines else text)
    if "error" in payload:
        raise RuntimeError(f"mcp {method}: {payload['error']}")
    result = payload.get("result", {})
    if result.get("isError"):
        raise RuntimeError(f"mcp {method}: {json.dumps(result.get('content'))[:200]}")
    if "structuredContent" in result:
        return result["structuredContent"]
    raw = (result.get("content") or [{}])[0].get("text", "{}")
    return json.loads(raw)


def _sql(stmt: str) -> list[dict]:
    return _post_json("/api/query/sql", {"stmt": stmt}) or []


def _db(action: str, **kw) -> dict:
    return _rpc("tools/call", {"name": "database", "arguments": {"action": action, **kw}})


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
    """Все database-блоки воркспейса: av-блок ядра (data-av-id=) + legacy i-носитель (avID=)."""
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


def pr_key_of(av_id: str) -> dict | None:
    keys = (_db("keys", id=av_id).get("data") or {}).get("keys") or []
    return next((key for key in keys if key.get("name") == "PR"), None)


def render_page(av_id: str, block_id: str, page: int) -> tuple[list[tuple[str, dict]], dict]:
    """(item_id, PR-значение) одной страницы render + view (для rowCount/rows)."""
    data = _db("render", id=av_id, blockID=block_id, page=page, pageSize=PAGE_SIZE).get("data") or {}
    view = data.get("view") or {}
    key_names = {c.get("id"): c.get("name") for c in view.get("columns") or []}
    cells: list[tuple[str, dict]] = []
    for row in view.get("rows") or []:
        # itemID для item_update — id строки; kernel GetBlockValue матчит v.BlockID == itemID
        row_id = row.get("id") or (row.get("cells") or [{}])[0].get("value", {}).get("blockID", "")
        for cell in row.get("cells") or []:
            value = cell.get("value") or {}
            if key_names.get(value.get("keyID")) == "PR":
                cells.append((row_id, value))
    return cells, view


def render_all(av_id: str, block_id: str) -> list[tuple[str, dict]]:
    """(item_id, PR-значение) всех строк, по страницам render."""
    cells: list[dict] = []
    page = 1
    while True:
        page_cells, view = render_page(av_id, block_id, page)
        cells.extend(page_cells)
        total = view.get("rowCount") or len(cells)
        if page * PAGE_SIZE >= total or not view.get("rows"):
            return cells
        page += 1


def snapshot_cell(item_id: str, value: dict) -> dict:
    return {
        "row": item_id,
        "type": value.get("type"),
        "text": (value.get("text") or {}).get("content", ""),
        "url": (value.get("url") or {}).get("content", ""),
    }


def collect_boards() -> list[dict]:
    """Инвентаризация: доски с ключом PR + снапшот значений (до изменений)."""
    docs = board_docs()
    boards: list[dict] = []
    for block in find_av_blocks():
        key = pr_key_of(block["av_id"])
        if not key:
            continue
        cells = [snapshot_cell(item_id, v) for item_id, v in render_all(block["av_id"], block["block_id"])]
        root_rows = _sql(_sql_by_id("root_id", block["block_id"]))
        root = root_rows[0]["root_id"] if root_rows else ""
        boards.append({
            "av_id": block["av_id"],
            "block_id": block["block_id"],
            "doc": docs.get(root, "?"),
            "key": key,
            "cells": cells,
        })
    return boards


def backup(boards: list[dict], backup_dir: Path) -> None:
    """render-снапшот + копия av-файлов. Каталог бэкапа — вне воркспейса: его
    файлы принадлежат контейнерному uid, запись в workspace недоступна."""
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


def migrate(boards: list[dict]) -> tuple[int, int, int]:
    """key_update text→url + перезапись url из старого text. Возвращает (конвертировано, перезаписано, уже-url)."""
    converted = refilled = already_url = 0
    for board in boards:
        key_id = board["key"]["id"]
        if board["key"]["type"] == "text":
            _db("key_update", id=board["av_id"], keyID=key_id, config={"type": "url"})
            converted += 1
            time.sleep(0.3)
        else:
            already_url += 1
        for cell in board["cells"]:
            # ячейки сняты ДО конвертации: на text-досках у них type=text,
            # критерий заполнения — по содержимому, не по типу
            if cell["url"] == "" and cell["text"]:
                _db(
                    "item_update",
                    id=board["av_id"],
                    keyID=key_id,
                    itemID=cell["row"],
                    value={"url": {"content": cell["text"]}},
                )
                refilled += 1
                time.sleep(0.3)
    return converted, refilled, already_url


def verify(boards: list[dict]) -> list[str]:
    """Повторный render: каждая заполненная ячейка — url-тип со прежним значением."""
    mismatches: list[str] = []
    for board in boards:
        after = {item_id: snapshot_cell(item_id, v) for item_id, v in render_all(board["av_id"], board["block_id"])}
        for before in board["cells"]:
            if not before["text"] and not before["url"]:
                continue
            now = after.get(before["row"])
            # уже url-заполненные не трогались, пустой url перезаписан из text
            expected_url = before["url"] or before["text"]
            if now is None:
                mismatches.append(f"{board['doc']}: строка {before['row']} пропала")
            elif now["type"] != "url" or now["url"] != expected_url:
                mismatches.append(
                    f"{board['doc']}: {before['row']} type={now['type']} url={now['url']!r} (ожидался {expected_url!r})"
                )
    return mismatches


def dry_run(boards: list[dict], backup_dir: Path) -> None:
    backup_dir.mkdir(parents=True, exist_ok=True)
    report = {b["av_id"]: b["cells"] for b in boards}
    (backup_dir / "dryrun-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=1))
    for b in boards:
        stale = sum(1 for c in b["cells"] if c["type"] == "url" and c["url"] == "" and c["text"])
        suffix = f", осиротевших {stale}" if stale else ""
        print(f"  {b['doc']} [{b['key']['type']}] ячеек {len(b['cells'])}{suffix}")
    print("dry-run: снапшот в", backup_dir)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="только отчёт, без изменений")
    parser.add_argument("--backup-dir", default=None, help="каталог бэкапа (деф. ~/siyuan-backups/av-pr-url-<ts>)")
    parser.add_argument("--only", metavar="AV_ID", help="мигрировать только эту database (smoke/точечный прогон)")
    args = parser.parse_args()

    boards = collect_boards()
    if args.only:
        boards = [b for b in boards if b["av_id"] == args.only]
        if not boards:
            print(f"database {args.only} не найдена или без ключа PR", file=sys.stderr)
            return 2
    filled = sum(1 for b in boards for c in b["cells"] if c["text"] or c["url"])
    print(f"досок с ключом PR: {len(boards)}, заполненных ячеек: {filled}")

    if args.dry_run:
        backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / "siyuan-backups/av-pr-url-dryrun"
        dry_run(boards, backup_dir)
        return 0

    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / f"siyuan-backups/av-pr-url-{ts}"
    backup(boards, backup_dir)
    print("бэкап:", backup_dir)

    converted, refilled, already_url = migrate(boards)
    print(f"конвертировано ключей: {converted}, перезаписано значений: {refilled}, уже url: {already_url}")

    mismatches = verify(boards)
    if mismatches:
        print("РАСХОЖДЕНИЯ:", file=sys.stderr)
        for line in mismatches:
            print(" ", line, file=sys.stderr)
        return 1
    print("верификация: все заполненные PR-ячейки type=url, url.content == прежний text")
    return 0


if __name__ == "__main__":
    try:
        _rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "migrate-pr-url", "version": "1.0"}})
        _rpc("notifications/initialized")
        sys.exit(main())
    except urllib.error.URLError as error:
        print(f"SiYuan недоступен: {error}", file=sys.stderr)
        sys.exit(2)
