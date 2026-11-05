#!/usr/bin/env python3
"""wiki_ops — операции с доками uwuwu (SiYuan-бекенд).

Один файл по контракту: dreamer/triage ссылаются на путь tools/wiki_ops.py напрямую
(см. dreamer/dreamer.ts) — разнесение по модулям сломает их пайплайны.

Команды (совместимы с прежним интерфейсом дример-агентов):
  export <docId>                     markdown дока по id
  create "Заголовок" <файл> --parent <hpath>   создать док (ноутбук Projects)
  append <docId> --md <файл>         дописать markdown-блоки в конец
  replace <docId> --md <файл>        заменить док целиком
  delete <docId> [--force]           снести док в корзину SiYuan (восстановимо)
  orgls                              дерево: ноутбуки + подпапки
  find "тайтл"                       поиск дока по заголовку

Блочная хирургия — точечная правка rich-доков по blockID (doc-level append умеет
только «в конец», replace пересоздаёт док и теряет привязки/живые HTML-блоки):
  block-update <blockID> --md <файл>     правка блока; payload строго один блок —
                                         multi-block ядро молча обрезает до первого
  block-insert-after <blockID> --md <файл>   вставка после блока (multi-block легален)
  block-delete <blockID>                 снос leaf-блока; контейнер с детьми — отказ
                                         (ядро сносит поддерево целиком и тихо;
                                         снос list — update контейнера: list→параграф;
                                         item list ядро не оставляет без ребёнка —
                                         посерийный снос item'ов невозможен)

Guard'ы блочных команд до любой мутации: формат id до любого SQL; блок резолвится
в индексе (LIMIT 1); doc-id (type='d') — отказ всем трём (update на doc-id молча
стирает весь док; правка/снос дока — append/replace/delete). Каждый stmt несёт
явный LIMIT. Мутация — один сетевой вызов (post attempts=1) без авто-ретрая:
ретрай после потери ответа — класс дубля вставки, не самоисправление. Undo-журнал
пишется ДО мутации через kernel putFile в data/assets/undo/wiki_ops/<unixts>-<op>-<id>.json
(before_md / after_md / payload_md / new_ids): ядро пишет файл под своим uid — прямой
FS-доступ с хоста к data/assets невозможен (проба 06.11), зато журнал работает с любой
машины, где доступно ядро. Не записался — warning, операция не падает.

delete: без --force сносится только док-заглушка дубля. Заглушка — строка-цитата
markdown, у которой на ОДНОЙ строке есть и «⚠️», и слово «канон»/«заглушк»
(без учёта регистра); смежные строки не объединяются. Guard'ы: док с дочерними
доками сносится только с --force (subtree уходит целиком); канон (без маркера)
— только с --force. ВНИМАНИЕ: --force сносит subtree, доски и тела задач —
для тел задач правильный путь uwuwu-cli task rm, а не delete.

orgls: каждый stmt несёт явный limit (ORG_LS_LIMIT) — ядро SiYuan без LIMIT
молча отдаёт первые 64 строки (см. tests/test_wiki_ops_orgls.py).

Конфиг: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) + UWUWU_SIYUAN_TOKEN
(или UWUWU_SIYUAN_WS/conf/conf.json).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

URL = None
TOKEN = None

# Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки stmt
# (04.11: Experience 859 доков, orgls видел 64). Явный предел обязателен в каждом stmt.
ORG_LS_LIMIT = 100000

# Формат docId ядра SiYuan: 14 цифр (timestamp) + '-' + 7 символов [a-z0-9].
# Валидация до любых SQL-запросов — инъекция/мусор отсекаются на входе.
DOC_ID_RE = re.compile(r"^\d{14}-[a-z0-9]{7}$")


def _conf():
    global URL, TOKEN
    URL = (URL or os.environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")).rstrip("/")
    if TOKEN:
        return
    tok = os.environ.get("UWUWU_SIYUAN_TOKEN", "")
    if not tok:
        ws = os.environ.get("UWUWU_SIYUAN_WS", str(Path.home() / "siyuan-demo-ws"))
        conf = json.loads((Path(ws) / "conf" / "conf.json").read_text())
        tok = conf["api"]["token"]
    TOKEN = tok


def post(path: str, body: dict, attempts: int = 6):
    _conf()
    last = "?"
    for i in range(attempts):
        req = urllib.request.Request(URL + path, data=json.dumps(body).encode(),
                                     headers={"Authorization": f"Token {TOKEN}",
                                              "Content-Type": "application/json"}, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                d = json.loads(r.read())
            if d.get("code") == 0:
                return d.get("data")
            last = f"code={d.get('code')} msg={d.get('msg')}"
            if "metadata is not ready" in str(d.get("msg")):
                time.sleep(2 + i)
                continue
        except Exception as e:
            last = str(e)
        time.sleep(1 + i)
    raise RuntimeError(f"POST {path}: {last}")


def notebooks(attempts: int = 6) -> list[dict]:
    return [nb for nb in post("/api/notebook/lsNotebooks", {}, attempts=attempts).get("notebooks", [])
            if not nb.get("closed")]


def cmd_export(a):
    r = post("/api/export/exportMdContent", {"id": a.docId})
    print((r or {}).get("content", ""))


def cmd_create(a):
    md = Path(a.file).read_text()
    nb = next((n["id"] for n in notebooks() if n["name"] == "Projects"), None)
    if not nb:
        raise RuntimeError("ноутбук Projects не найден")
    hpath = a.parent.rstrip("/") + "/" + a.title.strip("/")
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"создан: {hpath}")


def cmd_append(a):
    md = Path(a.file).read_text()
    post("/api/block/appendBlock", {"dataType": "markdown", "data": md, "parentID": a.docId})
    print(f"дописано в {a.docId}")


def cmd_replace(a):
    doc_id = a.docId.strip()
    if not DOC_ID_RE.fullmatch(doc_id):
        raise RuntimeError(f"неверный docId: {doc_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    md = Path(a.file).read_text()
    # найти док → пересоздать
    stmt = f"select hpath, box from blocks where type='d' and id='{doc_id}' limit 1"  # nosec B608 — docId валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    if not rows:
        raise RuntimeError("док не найден")
    hpath, box = rows[0]["hpath"], rows[0]["box"]
    nb_name = next((n["name"] for n in notebooks() if n["id"] == box), "Projects")
    post("/api/filetree/removeDocByID", {"id": a.docId})
    time.sleep(1)
    nb = next(n["id"] for n in notebooks() if n["name"] == nb_name)
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"заменён: {hpath}")


def _sql_rows(stmt: str) -> list[dict]:
    rows = post("/api/query/sql", {"stmt": stmt})
    return rows if isinstance(rows, list) else rows.get("data", [])


def _sql_scalar(stmt: str) -> int:
    rows = _sql_rows(stmt)
    return int(rows[0]["c"]) if rows else 0


def _sql_str(value: str) -> str:
    # Одинарная кавычка в SQL-литерале экранируется удвоением (SQLite kernel).
    return value.replace("'", "''")


def _doc_row(doc_id: str) -> dict | None:
    stmt = f"select id, hpath, path, box from blocks where type='d' and id='{doc_id}' limit 1"  # nosec B608 — docId валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    return rows[0] if rows else None


def _children_count(doc_path: str) -> int:
    # Формат blocks.path: сегменты-директории без «.sy», расширение только у листа
    # (/…/<parent>/<child>.sy). Проверено на живом ядре: сегменты — только id.
    parent_prefix = doc_path.removesuffix(".sy")
    stmt = f"select count(*) c from blocks where type='d' and path like '{parent_prefix}/%' limit 1"  # nosec B608 — path из kernel API, только id-сегменты
    return _sql_scalar(stmt)


def _siblings_count(box: str, hpath: str) -> int:
    hpath_sql = _sql_str(hpath)
    stmt = f"select count(*) c from blocks where type='d' and box='{box}' and hpath='{hpath_sql}' limit 1"  # nosec B608 — box из kernel API, hpath экранирован _sql_str
    return _sql_scalar(stmt)


def _is_stub(doc_id: str) -> bool:
    # Заглушка дубля: строка-цитата с «⚠️» И словом «канон»/«заглушк» на той же строке.
    r = post("/api/export/exportMdContent", {"id": doc_id})
    content = (r or {}).get("content", "")
    for line in content.splitlines():
        if not line.lstrip().startswith(">"):
            continue
        low = line.lower()
        if "⚠️" in line and ("канон" in low or "заглушк" in low):
            return True
    return False


def _notebook_display_name(box_id: str) -> str:
    # Косметика для трейла delete: один запрос без ретраев, отказ ядра
    # не роняет команду после успешного сноса — фолбэк на box-id.
    try:
        return next(n["name"] for n in notebooks(attempts=1) if n["id"] == box_id)
    except Exception:
        return box_id


def cmd_delete(a):
    doc_id = a.docId.strip()
    if not DOC_ID_RE.fullmatch(doc_id):
        raise RuntimeError(f"неверный docId: {doc_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    row = _doc_row(doc_id)
    if not row:
        raise RuntimeError(f"док {doc_id} не найден (уже удалён?) — проверь wiki_ops find")
    children = _children_count(row["path"])
    if children and not a.force:
        raise RuntimeError(f"у дока «{row['hpath']}» есть дочерние доки ({children}) — снос унесёт subtree; подтверди --force")
    siblings = _siblings_count(row["box"], row["hpath"])
    stub = _is_stub(doc_id)
    if not stub and not a.force:
        if siblings <= 1:
            raise RuntimeError(f"«{row['hpath']}» — единственный док по пути и без маркера заглушки (канон); снос только с --force")
        raise RuntimeError(f"док без маркера заглушки; по пути ещё {siblings - 1} дубл(ей) — погаси заглушкой (replace + маркер «⚠️ … Канон — …») или подтверди --force")
    post("/api/filetree/removeDocByID", {"id": doc_id}, attempts=2)
    for _ in range(2):
        if _doc_row(doc_id) is None:
            marker = "да" if stub else "нет (--force)"
            print(f"снесён: {doc_id} {row['hpath']} (ноутбук {_notebook_display_name(row['box'])})")
            print(f"  маркер заглушки: {marker}; соседей по пути: {siblings}; дочерних доков: {children}")
            print("  → корзина SiYuan, восстановимо через UI (история данных) до истечения срока хранения")
            return
        time.sleep(1)
    raise RuntimeError("removeDocByID вернул ok, но док остался в индексе — проверь состояние дока вручную")


# --- блочная хирургия: block-update / block-insert-after / block-delete ---

def _block_row(block_id: str) -> dict | None:
    stmt = f"select id, type, root_id, parent_id, markdown from blocks where id='{block_id}' limit 1"  # nosec B608 — blockID валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    return rows[0] if rows else None


def _block_children_count(parent_id: str) -> int:
    stmt = f"select count(*) c from blocks where parent_id='{parent_id}' limit 1"  # nosec B608 — parent_id из kernel API (валидирован на входе)
    return _sql_scalar(stmt)


def _root_hpath(root_id: str) -> str:
    stmt = f"select hpath from blocks where type='d' and id='{root_id}' limit 1"  # nosec B608 — root_id из kernel API
    rows = _sql_rows(stmt)
    return rows[0]["hpath"] if rows else ""


def _resolved_block(block_id: str) -> dict:
    """Формат id до любого SQL (инъекция/мусор отсечены), резолв в индексе,
    doc-id (type='d') — не блок: update на doc-id молча стирает весь док."""
    if not DOC_ID_RE.fullmatch(block_id):
        raise RuntimeError(f"неверный blockID: {block_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    row = _block_row(block_id)
    if not row:
        raise RuntimeError(f"блок {block_id} не найден в индексе — возьми id из wiki_ops export или SQL по доку")
    if row["type"] == "d":
        raise RuntimeError("doc-id не блок: правка контента дока — append/replace, снос дока — wiki_ops delete")
    return row


def _mutate(path: str, body: dict) -> dict:
    """Мутация блока — один сетевой вызов без авто-ретрая: ретрай после потери
    ответа даёт дубль вставки (не самоисправляется). Ошибка несёт рецепт проверки
    состояния до повтора."""
    try:
        return post(path, body, attempts=1)
    except RuntimeError as exc:
        raise RuntimeError(f"{exc}; применение не подтверждено — проверь блок (export/SQL) до повтора операции") from None


FENCE_RE = re.compile(r"^(`{3,}|~{3,}).*$")


def _is_closing_fence(line: str, fence_char: str) -> bool:
    return line[0] == fence_char and set(line) == {fence_char}


def _advance_plain_line(line: str, inside_chunk: bool) -> tuple[int, bool]:
    """Плоская строка вне кода: пустая закрывает чанк, непустая после разрыва
    открывает новый. Возвращает (дельта чанков, inside_chunk)."""
    if not line:
        return 0, False
    return (1, True) if not inside_chunk else (0, True)


def _payload_chunk_count(markdown: str) -> int:
    """Число топ-уровневых блоков в markdown: фрагменты, разделённые пустой строкой;
    fenced-код держит пустые строки внутри одного блока. Ошибки парсера уходят
    в безопасную сторону — лишний отказ вместо тихой потери хвоста."""
    chunk_count = 0
    in_code = False
    fence_char = ""
    inside_chunk = False
    for raw_line in markdown.splitlines():
        line = raw_line.strip()
        if FENCE_RE.match(line):
            if not in_code:
                in_code, fence_char = True, line[0]
                if not inside_chunk:
                    chunk_count += 1
            elif _is_closing_fence(line, fence_char):
                in_code = False
            inside_chunk = True
            continue
        if in_code:
            inside_chunk = True
            continue
        delta, inside_chunk = _advance_plain_line(line, inside_chunk)
        chunk_count += delta
    return chunk_count


UNDO_DIR = "data/assets/undo/wiki_ops"


def _put_workspace_file(ws_path: str, payload: bytes) -> None:
    """Запись файла в воркспейс, контракт ядра 3.8.9: POST multipart (path от
    корня воркспейса + поле file), ядро создаёт недостающие директории и пишет
    под своим uid — прямой FS-доступ с хоста к data/assets невозможен (проба
    06.11: EACCES, владелец — контейнер). Тот же канал, что undo-журнал cli."""
    _conf()
    # B310: urlopen разрешён только для http/https из UWUWU_SIYUAN_URL.
    if urlparse(URL).scheme not in {"http", "https"}:
        raise RuntimeError(f"putFile: недопустимая схема в UWUWU_SIYUAN_URL: {URL!r}")
    boundary = "----wiki-ops-undo-journal"
    body = b"".join([
        f"--{boundary}\r\n".encode(),
        b'Content-Disposition: form-data; name="path"\r\n\r\n',
        f"{ws_path}\r\n".encode(),
        f"--{boundary}\r\n".encode(),
        b'Content-Disposition: form-data; name="file"; filename="undo.json"\r\n',
        b"Content-Type: application/octet-stream\r\n\r\n",
        payload,
        f"\r\n--{boundary}--\r\n".encode(),
    ])
    req = urllib.request.Request(URL + "/api/file/putFile", data=body,
                                 headers={"Authorization": f"Token {TOKEN}",
                                          "Content-Type": f"multipart/form-data; boundary={boundary}"},
                                 method="POST")
    with urllib.request.urlopen(req, timeout=60) as r:  # nosec B310 — схема http/https проверена выше; URL из UWUWU_SIYUAN_URL ядра
        d = json.loads(r.read())
    if d.get("code") != 0:
        raise RuntimeError(f"putFile: code={d.get('code')} msg={d.get('msg')}")


def _undo_write(operation: str, block_id: str, payload: dict) -> dict | None:
    """Undo-журнал ДО мутации (write-ahead), через kernel putFile — работает с
    любой машины, где доступно ядро. Деградация (ядро отказало) не валит
    операцию — warning + рецепт отката руками. None — журнал не записан."""
    try:
        now = int(time.time())
        name = f"{now}-{operation}-{block_id}.json"
        entry = {"ts": now, "op": operation, "id": block_id, **payload}
        _put_workspace_file(f"{UNDO_DIR}/{name}",
                            json.dumps(entry, ensure_ascii=False, indent=2).encode())
        entry["_name"] = name
        return entry
    except Exception as exc:
        print(f"WARNING: undo-журнал не записан ({exc}); откат — руками: контент из wiki_ops export дока")
        return None


def _undo_patch(journal: dict | None, fields: dict) -> None:
    """Дописать поля (new_ids), известные только после мутации, в записанный журнал."""
    if not journal:
        return
    name = journal.pop("_name", None)
    if not name:
        return
    try:
        journal.update(fields)
        _put_workspace_file(f"{UNDO_DIR}/{name}",
                            json.dumps(journal, ensure_ascii=False, indent=2).encode())
    except Exception as exc:
        print(f"WARNING: undo-журнал не дополнен ({exc})")


def _new_block_ids(resp) -> list[str]:
    """Новые block id из doOperations[].data (data-node-id) — для цепочки операций.
    Форма ответа ядра зависит от эндпоинта: updateBlock — dict, insertBlock —
    список транзакций (проба 06.11)."""
    ops: list[dict] = []
    if isinstance(resp, list):
        ops = [op for tx in resp for op in tx.get("doOperations", [])]
    elif isinstance(resp, dict):
        ops = resp.get("doOperations", [])
    block_ids: list[str] = []
    for op in ops:
        block_ids += re.findall(r'data-node-id="(\d{14}-[a-z0-9]{7})"', op.get("data") or "")
    return block_ids


def _read_payload(filename: str) -> str:
    payload = Path(filename).read_text()
    if not payload.strip():
        raise RuntimeError("пустой payload — блок нельзя обновить/вставить пустотой (снос — block-delete)")
    return payload


def cmd_block_update(a):
    payload = _read_payload(a.file)
    row = _resolved_block(a.blockID.strip())
    chunks = _payload_chunk_count(payload)
    if chunks > 1:
        raise RuntimeError(f"payload содержит {chunks} блока(ов) — updateBlock молча применит только первый, хвост пропадёт (проба 06.11); для нескольких блоков — block-insert-after; многострочный контент одного блока — параграф с переносами или код-блок")
    journal = _undo_write("update", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "before_md": row["markdown"], "after_md": payload,
    })
    _mutate("/api/block/updateBlock", {"dataType": "markdown", "data": payload, "id": row["id"]})
    print(f"обновлён блок {row['id']} (тип {row['type']})")
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']}")
        print("  откат: block-update этого id с before_md из журнала")


def cmd_block_insert_after(a):
    payload = _read_payload(a.file)
    row = _resolved_block(a.blockID.strip())
    journal = _undo_write("insert-after", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "payload_md": payload,
    })
    resp = _mutate("/api/block/insertBlock", {"dataType": "markdown", "data": payload, "previousID": row["id"]})
    new_ids = _new_block_ids(resp)
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']} (откат: block-delete новых блоков)")
    _undo_patch(journal, {"new_ids": new_ids})
    print(f"вставлено после {row['id']} (тип {row['type']}), новых блоков: {len(new_ids)}")
    for block_id in new_ids:
        print(f"  новый блок: {block_id}")


def cmd_block_delete(a):
    row = _resolved_block(a.blockID.strip())
    children = _block_children_count(row["id"])
    if children:
        raise RuntimeError(f"блок {row['id']} — контейнер: {children} дет.(ей) уйдут вместе с ним тихо (проба 06.11: deleteBlock на list/blockquote сносит поддерево); снос контейнера — конверсией: block-update с одно-блочным payload (проба 06.11: list→параграф) или doc-level replace; посерийный снос item'ов list невозможен — ядро держит пустого ребёнка в каждом item")
    journal = _undo_write("delete", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "before_md": row["markdown"],
    })
    _mutate("/api/block/deleteBlock", {"id": row["id"]})
    print(f"снесён блок {row['id']} (тип {row['type']})")
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']}")
        print(f"  откат: block-insert-after на предыдущий сиблинг с before_md из журнала (позицию сверь по export {row['root_id']})")


def cmd_orgls(a):
    for nb in notebooks():
        print(f"[{nb['name']}]")
        stmt = f"select hpath from blocks where type='d' and box='{nb['id']}' limit {ORG_LS_LIMIT}"  # nosec B608 — nb.id из kernel API
        rows = _sql_rows(stmt)
        for r in sorted(rows, key=lambda x: x["hpath"]):
            depth = r["hpath"].count("/") - 1
            print("  " * (depth + 1) + r["hpath"].rstrip("/").split("/")[-1])


def cmd_find(a):
    stmt = f"select id, hpath from blocks where type='d' and content like '%{_sql_str(a.title)}%' limit 10"  # nosec B608 — title экранирован _sql_str
    rows = _sql_rows(stmt)
    for r in rows:
        print(r["id"], r["hpath"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("export"); p.add_argument("docId"); p.set_defaults(fn=cmd_export)
    p = sub.add_parser("create"); p.add_argument("title"); p.add_argument("file"); p.add_argument("--parent", default=""); p.set_defaults(fn=cmd_create)
    p = sub.add_parser("append"); p.add_argument("docId"); p.add_argument("--md", dest="file", required=True); p.set_defaults(fn=cmd_append)
    p = sub.add_parser("replace"); p.add_argument("docId"); p.add_argument("--md", dest="file", required=True); p.set_defaults(fn=cmd_replace)
    p = sub.add_parser("delete")
    p.add_argument("docId")
    p.add_argument("--force", action="store_true")
    p.set_defaults(fn=cmd_delete)
    p = sub.add_parser("block-update")
    p.add_argument("blockID")
    p.add_argument("--md", dest="file", required=True)
    p.set_defaults(fn=cmd_block_update)
    p = sub.add_parser("block-insert-after")
    p.add_argument("blockID")
    p.add_argument("--md", dest="file", required=True)
    p.set_defaults(fn=cmd_block_insert_after)
    p = sub.add_parser("block-delete")
    p.add_argument("blockID")
    p.set_defaults(fn=cmd_block_delete)
    p = sub.add_parser("orgls"); p.set_defaults(fn=cmd_orgls)
    p = sub.add_parser("find"); p.add_argument("title"); p.set_defaults(fn=cmd_find)
    a = ap.parse_args()
    try:
        a.fn(a)
    except RuntimeError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
