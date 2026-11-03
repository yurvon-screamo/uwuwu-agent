"""BIND: привязка строк канбана к телам-докам.

Заменяет detached-строки на bound (blockID = тело-док): клик по карточке
открывает документ, связь строка↔тело настоящая, не по тайтлу.

Для каждой доски: render строк → если есть тело-саб-док с тайтлом строки и
строка detached → item_remove + item_add(blockID=тело) + вернуть статусы.
"""
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from siyuan_migrate.boards import SiYuanMcp
from siyuan_migrate.siyuan_client import SiYuan


def path_safe(t: str) -> str:
    return t.replace("/", "∕")


def av_and_carrier(sy, doc_id):
    rows = sy.sql(f"select id, markdown from blocks where root_id='{doc_id}' and type='av'")
    for r in rows:
        m = re.search(r'data-av-id="([0-9]{14}-[a-z0-9]+)"', r.get("markdown") or "")
        if m:
            return m.group(1), r.get("id", "")
    return "", ""


def render_rows(mcp, av):
    v = mcp.tool("database", {"action": "render", "id": av})
    d = v.get("data", v)
    cols = {c.get("id"): c.get("name") for c in (d.get("view", {}).get("columns") or [])}
    out = []
    for row in d.get("view", {}).get("rows") or []:
        title, cells = "", {}
        for c in row.get("cells", []):
            val = c.get("value", {})
            col = cols.get(val.get("keyID", ""), "")
            if val.get("type") == "block":
                title = (val.get("block") or {}).get("content", "")
                detached = val.get("isDetached", False)
            else:
                v_ = ""
                if val.get("mSelect"):
                    v_ = ",".join(o.get("content", "") for o in val["mSelect"])
                elif val.get("text"):
                    v_ = val["text"].get("content", "")
                if col:
                    cells[col] = v_
        out.append({"id": row.get("id", ""), "title": title, "detached": detached, "cells": cells})
    return out


def bind_board(sy, mcp, proj, title, dry=False):
    nb = sy.ensure_notebook("Projects")
    board_h = f"/{proj}/{title}"
    doc = sy.get_doc_id_by_path(nb, board_h)
    if not doc:
        return {"board": title, "error": "no board doc"}
    av, _ = av_and_carrier(sy, doc)
    if not av:
        return {"board": title, "error": "no db"}
    keys = mcp.tool("database", {"action": "keys", "id": av})
    arr = (keys.get("data", {}) or {}).get("keys") or keys.get("keys") or []
    keyid = {k["name"]: k["id"] for k in arr if k.get("name")}
    bound = rebound = missing = 0
    for row in render_rows(mcp, av):
        if not row["title"]:
            continue
        if not row["detached"]:
            bound += 1
            continue
        body = sy.get_doc_id_by_path(nb, f"{board_h}/{path_safe(row['title'])}")
        if not body:
            missing += 1
            continue
        if dry:
            rebound += 1
            continue
        # перепривязка: удалить detached-строку, создать bound к телу
        try:
            mcp.tool("database", {"action": "item_remove", "id": av, "itemIDs": [row["id"]]})
        except Exception as e:
            print("  rm fail", row["title"][:30], str(e)[:50])
            continue
        time.sleep(0.3)
        try:
            item = mcp.tool("database", {"action": "item_add", "id": av, "blockID": body})
        except Exception as e:
            print("  add fail", row["title"][:30], str(e)[:60])
            continue
        new_id = (item.get("data", {}) or {}).get("itemID", "")
        rebound += 1
        time.sleep(0.3)
        # вернуть клетки
        for name, val in row["cells"].items():
            if not val or not keyid.get(name):
                continue
            typed = ({"mSelect": [{"content": val}]}
                     if name in ("Status",)
                     else {"text": {"content": val}})
            try:
                mcp.tool("database", {"action": "item_update", "id": av,
                                      "keyID": keyid[name], "itemID": new_id, "value": typed})
            except Exception as e:
                print("  cell fail", name, str(e)[:50])
            time.sleep(0.2)
    return {"board": title, "bound": bound, "rebound": rebound, "no_body": missing}


def main():
    dry = "--dry" in sys.argv
    only = ""
    for a in sys.argv[1:]:
        if a.startswith("--only="):
            only = a.split("=", 1)[1]
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    mcp = SiYuanMcp("http://127.0.0.1:6806", sy.token)
    mcp.initialize()
    projs = [only] if only else ["uwuwu", "origa", "foilcap", "1xgames", "true-washing"]
    for proj in projs:
        for t in ([f"Задачи {proj}"] if only else [f"Задачи {proj}", f"Кроны {proj}"]):
            r = bind_board(sy, mcp, proj, t, dry=dry)
            print(r, flush=True)


if __name__ == "__main__":
    main()
