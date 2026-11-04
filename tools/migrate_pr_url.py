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

_ID = [0]
_SID = None


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
    with urllib.request.urlopen(req, timeout=60) as resp:
        sid = resp.headers.get("Mcp-Session-Id")
        if sid:
            _SID = sid
        if method.startswith("notifications/"):
            return {}
        text = resp.read().decode()
    for line in text.splitlines():
        if line.startswith("data: "):
            text = line[6:]
            break
    payload = json.loads(text)
    if "error" in payload:
        raise RuntimeError(f"mcp {method}: {payload['error']}")
    result = payload.get("result", {})
    if result.get("isError"):
        raise RuntimeError(f"mcp {method}: {json.dumps(result.get('content'))[:200]}")
    if "structuredContent" in result:
        return result["structuredContent"]
    raw = (result.get("content") or [{}])[0].get("text", "{}")
    return json.loads(raw)


def _token() -> str:
    conf = json.loads((WS / "conf/conf.json").read_text())
    return conf["api"]["token"]


def _base() -> str:
    import os

    return os.environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")


def _http(path: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"{_base()}{path}",
        json.dumps(body).encode(),
        {"Authorization": f"Token {_token()}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        payload = json.load(resp)
    if payload.get("code") != 0:
        raise RuntimeError(f"{path}: {payload}")
    return payload.get("data") or {}


def _sql(stmt: str) -> list[dict]:
    return _http("/api/query/sql", {"stmt": stmt}) or []


def _db(action: str, **kw) -> dict:
    return _rpc("tools/call", {"name": "database", "arguments": {"action": action, **kw}})


def _workspace() -> Path:
    import os

    return Path(os.environ.get("UWUWU_SIYUAN_WS", str(WS)))


def find_av_blocks() -> list[dict]:
    """Все database-блоки воркспейса: av-блок ядра (data-av-id=) + legacy i-носитель (avID=)."""
    seen: set[str] = set()
    out: list[dict] = []
    for stmt in (
        "select id, markdown, root_id from blocks where type='av' and markdown like '%av-id=%' limit 500",
        "select id, markdown, root_id from blocks where type='i' and markdown like '%avID=%' limit 500",
    ):
        for block in _sql(stmt):
            markdown = block.get("markdown") or ""
            match = re.search(r'data-av-id="([0-9]{14}-[a-z0-9]+)"', markdown) or re.search(
                r'avID="([0-9]{14}-[a-z0-9]+)"', markdown
            )
            if not match or match.group(1) in seen:
                continue
            seen.add(match.group(1))
            out.append({"av_id": match.group(1), "block_id": block["id"]})
    return out


def board_docs() -> dict[str, str]:
    docs = _sql("select id, hpath from blocks where type='d' limit 3000")
    return {d["id"]: d.get("hpath", "?") for d in docs}


def pr_key_of(av_id: str) -> dict | None:
    keys = (_db("keys", id=av_id).get("data") or {}).get("keys") or []
    for key in keys:
        if key.get("name") == "PR":
            return key
    return None


def render_all(av_id: str, block_id: str) -> tuple[dict, list[dict]]:
    """Все строки по страницам; возвращает (view, pr_cells)."""
    rows: list[dict] = []
    view: dict = {}
    page = 1
    while True:
        data = _db("render", id=av_id, blockID=block_id, page=page, pageSize=PAGE_SIZE).get("data") or {}
        view = data.get("view") or {}
        batch = view.get("rows") or []
        rows.extend(batch)
        total = view.get("rowCount") or len(rows)
        if page * PAGE_SIZE >= total or not batch:
            break
        page += 1
    key_names = {c.get("id"): c.get("name") for c in view.get("columns") or []}
    cells: list[dict] = []
    for row in rows:
        for cell in row.get("cells") or []:
            value = cell.get("value") or {}
            if key_names.get(value.get("keyID")) == "PR":
                cells.append(value)
    return view, cells


def snapshot_cell(value: dict) -> dict:
    return {
        "row": value.get("id"),
        "type": value.get("type"),
        "text": (value.get("text") or {}).get("content", ""),
        "url": (value.get("url") or {}).get("content", ""),
    }


def backup_av_files(av_ids: list[str], backup_dir: Path) -> None:
    """Копия av-файлов. Каталог бэкапа — вне воркспейса: его файлы принадлежат
    контейнерному uid, запись в workspace пользователю недоступна."""
    av_dir = _workspace() / "data/storage/av"
    backup_dir.mkdir(parents=True, exist_ok=True)
    for av_id in av_ids:
        src = av_dir / f"{av_id}.json"
        if src.exists():
            shutil.copy2(src, backup_dir / f"{av_id}.json")


def save_render_snapshot(av_id: str, snapshot: dict, backup_dir: Path) -> None:
    (backup_dir / f"render-{av_id}.json").write_text(json.dumps(snapshot, ensure_ascii=False, indent=1))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="только отчёт, без изменений")
    parser.add_argument("--backup-dir", default=None, help="каталог бэкапа (деф. ~/siyuan-backups/av-pr-url-<ts>)")
    args = parser.parse_args()

    docs = board_docs()
    boards: list[dict] = []
    for block in find_av_blocks():
        av_id = block["av_id"]
        key = pr_key_of(av_id)
        if not key:
            continue
        _, cells = render_all(av_id, block["block_id"])
        boards.append({
            "av_id": av_id,
            "block_id": block["block_id"],
            "doc": docs.get(_root_of(av_id, block["block_id"]), "?"),
            "key": key,
            "cells": [snapshot_cell(c) for c in cells],
        })

    filled = sum(1 for b in boards for c in b["cells"] if c["text"] or c["url"])
    print(f"досок с ключом PR: {len(boards)}, заполненных ячеек: {filled}")

    if args.dry_run:
        report = {b["av_id"]: b["cells"] for b in boards}
        backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / "siyuan-backups/av-pr-url-dryrun"
        backup_dir.mkdir(parents=True, exist_ok=True)
        (backup_dir / "dryrun-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=1))
        for b in boards:
            stale = sum(1 for c in b["cells"] if c["type"] == "url" and c["url"] == "" and c["text"])
            print(f"  {b['doc']} [{b['key']['type']}] ячеек {len(b['cells'])}" + (f", осиротевших {stale}" if stale else ""))
        print("dry-run: снапшот в", backup_dir)
        return 0

    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_dir = Path(args.backup_dir) if args.backup_dir else Path.home() / f"siyuan-backups/av-pr-url-{ts}"
    backup_av_files([b["av_id"] for b in boards], backup_dir)
    for b in boards:
        save_render_snapshot(b["av_id"], {"cells": b["cells"], "key": b["key"]}, backup_dir)
    print("бэкап:", backup_dir)

    converted = 0
    refilled = 0
    skipped_boards = 0
    for b in boards:
        if b["key"]["type"] == "text":
            _db("key_update", id=b["av_id"], keyID=b["key"]["id"], config={"type": "url"})
            converted += 1
            time.sleep(0.3)
        else:
            skipped_boards += 1
        for cell in b["cells"]:
            # ячейки сняты ДО конвертации: на text-досках у них type=text,
            # критерий заполнения — по содержимому, не по типу
            if cell["url"] == "" and cell["text"]:
                _db(
                    "item_update",
                    id=b["av_id"],
                    keyID=b["key"]["id"],
                    itemID=cell["row"],
                    value={"url": {"content": cell["text"]}},
                )
                refilled += 1
                time.sleep(0.3)
    print(f"конвертировано ключей: {converted}, перезаписано значений: {refilled}, уже url: {skipped_boards}")

    mismatches: list[str] = []
    for b in boards:
        _, cells = render_all(b["av_id"], b["block_id"])
        after = {c.get("id"): snapshot_cell(c) for c in cells}
        for before in b["cells"]:
            if not before["text"] and not before["url"]:
                continue
            now = after.get(before["row"])
            # уже url-заполненные не трогались, пустой url перезаписан из text
            expected_url = before["url"] or before["text"]
            if now is None:
                mismatches.append(f"{b['doc']}: строка {before['row']} пропала")
            elif now["type"] != "url" or now["url"] != expected_url:
                mismatches.append(f"{b['doc']}: {before['row']} type={now['type']} url={now['url']!r} (ожидался {expected_url!r})")

    if mismatches:
        print("РАСХОЖДЕНИЯ:", file=sys.stderr)
        for line in mismatches:
            print(" ", line, file=sys.stderr)
        return 1
    print("верификация: все заполненные PR-ячейки type=url, url.content == прежний text")
    return 0


def _root_of(av_id: str, block_id: str) -> str:
    rows = _sql(f"select root_id from blocks where id='{block_id}' limit 1")
    return rows[0]["root_id"] if rows else ""


if __name__ == "__main__":
    try:
        _rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "migrate-pr-url", "version": "1.0"}})
        _rpc("notifications/initialized")
        sys.exit(main())
    except urllib.error.URLError as error:
        print(f"SiYuan недоступен: {error}", file=sys.stderr)
        sys.exit(2)
