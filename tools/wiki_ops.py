#!/usr/bin/env python3
"""wiki_ops — операции с доками uwuwu (SiYuan-бекенд).

Команды (совместимы с прежним интерфейсом дример-агентов):
  export <docId>                     markdown дока по id
  create "Заголовок" <файл> --parent <hpath>   создать док (ноутбук Projects)
  append <docId> --md <файл>         дописать markdown-блоки в конец
  replace <docId> --md <файл>        заменить док целиком
  orgls                              дерево: ноутбуки + подпапки
  find "тайтл"                       поиск дока по заголовку

orgls: каждый stmt несёт явный limit (ORG_LS_LIMIT) — ядро SiYuan без LIMIT
молча отдаёт первые 64 строки (см. tests/test_wiki_ops_orgls.py).

Конфиг: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) + UWUWU_SIYUAN_TOKEN
(или UWUWU_SIYUAN_WS/conf/conf.json).
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from pathlib import Path

URL = None
TOKEN = None

# Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки stmt
# (04.11: Experience 859 доков, orgls видел 64). Явный предел обязателен в каждом stmt.
ORG_LS_LIMIT = 100000


def _conf():
    global URL, TOKEN
    URL = (URL or __import__("os").environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")).rstrip("/")
    if TOKEN:
        return
    import os
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


def doc_id_by_path(nb: str, hpath: str) -> str | None:
    try:
        r = post("/api/filetree/getIDsByHPath", {"notebook": nb, "path": hpath}, attempts=2)
        return (r[0] if isinstance(r, list) and r else None)
    except Exception:
        return None


def notebooks() -> list[dict]:
    return [nb for nb in post("/api/notebook/lsNotebooks", {}).get("notebooks", [])
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
    md = Path(a.file).read_text()
    # найти док → пересоздать
    rows = post("/api/query/sql", {"stmt": f"select hpath, box from blocks where type='d' and id='{a.docId}'"})
    rows = rows if isinstance(rows, list) else rows.get("data", [])
    if not rows:
        raise RuntimeError("док не найден")
    hpath, box = rows[0]["hpath"], rows[0]["box"]
    nb_name = next((n["name"] for n in notebooks() if n["id"] == box), "Projects")
    post("/api/filetree/removeDocByID", {"id": a.docId})
    time.sleep(1)
    nb = next(n["id"] for n in notebooks() if n["name"] == nb_name)
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"заменён: {hpath}")


def cmd_orgls(a):
    for nb in notebooks():
        print(f"[{nb['name']}]")
        stmt = f"select hpath from blocks where type='d' and box='{nb['id']}' limit {ORG_LS_LIMIT}"
        rows = post("/api/query/sql", {"stmt": stmt})
        rows = rows if isinstance(rows, list) else rows.get("data", [])
        for r in sorted(rows, key=lambda x: x["hpath"]):
            depth = r["hpath"].count("/") - 1
            print("  " * (depth + 1) + r["hpath"].rstrip("/").split("/")[-1])


def cmd_find(a):
    stmt = f"select id, hpath from blocks where type='d' and content like '%{a.title.replace(chr(39), chr(39)*2)}%' limit 10"
    rows = post("/api/query/sql", {"stmt": stmt})
    rows = rows if isinstance(rows, list) else rows.get("data", [])
    for r in rows:
        print(r["id"], r["hpath"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("export"); p.add_argument("docId"); p.set_defaults(fn=cmd_export)
    p = sub.add_parser("create"); p.add_argument("title"); p.add_argument("file"); p.add_argument("--parent", default=""); p.set_defaults(fn=cmd_create)
    p = sub.add_parser("append"); p.add_argument("docId"); p.add_argument("--md", dest="file", required=True); p.set_defaults(fn=cmd_append)
    p = sub.add_parser("replace"); p.add_argument("docId"); p.add_argument("--md", dest="file", required=True); p.set_defaults(fn=cmd_replace)
    p = sub.add_parser("orgls"); p.set_defaults(fn=cmd_orgls)
    p = sub.add_parser("find"); p.add_argument("title"); p.set_defaults(fn=cmd_find)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
