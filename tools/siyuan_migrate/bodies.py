"""Экспорт+импорт ТЕЛ задач канбана (linked docs афины не входят в организ-дерево).

Для каждой строки каждой доски: export_doc_markdown(body_doc) → саб-док
/Projects/<proj>/<доска>/<title> в SiYuan + комменты секцией.
"""
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import wiki_ops
from siyuan_migrate.boards import (build_node_to_doc, comment_md,
                                   parse_row, read_board_rows, threads_for)
from siyuan_migrate.siyuan_client import SiYuan


def find_boards_manifest(manifest: dict, n2d: dict) -> list[dict]:
    out = []
    for node, info in manifest["docs"].items():
        if not info.get("ok"):
            continue
        name = Path(info["path"]).stem
        if name.startswith(("Задачи ", "Кроны ")):
            proj = Path(info["path"]).parts[-2]
            out.append({"doc_id": n2d.get(node, node), "title": name, "project": proj})
    return out


def export_body(sid, ws, doc_id: str, attempts: int = 3) -> tuple[str, str]:
    last = "?"
    for _ in range(attempts):
        r = wiki_ops.call(sid, "export_doc_markdown", {"workspaceId": ws, "docId": doc_id})
        for c in r.get("content", []):
            try:
                p = json.loads(c.get("text", ""))
            except Exception:
                continue
            if p.get("exists") is False:
                raise RuntimeError("missing")
            if p.get("exists") and (p.get("markdown") or "").strip():
                return str(p.get("title") or doc_id), p["markdown"]
            last = "empty"
        time.sleep(2)
    raise RuntimeError(f"export body failed: {last}")


def main() -> int:
    WS = "81475705-028a-4153-a8f5-f06e402ee31e"
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    nb = sy.ensure_notebook("Projects")
    sid = wiki_ops.session("siyuan-migrate-bodies")

    manifest = json.loads((Path.home() / "siyuan-migration-work" / "export" / "manifest.json").read_text())
    n2d = build_node_to_doc()
    doc_titles = {n2d.get(n, n): Path(i["path"]).stem for n, i in manifest["docs"].items() if i.get("ok")}
    boards = find_boards_manifest(manifest, n2d)
    print(f"boards: {[b['title'] for b in boards]}")

    stats = {"bodies": 0, "comments": 0, "no_body": 0, "fail": 0}
    for b in boards:
        try:
            rows = [parse_row(r, doc_titles) for r in read_board_rows(sid, b["doc_id"])]
        except Exception as e:
            print("rows FAIL", b["title"], str(e)[:80])
            continue
        board_hpath = f"/{b['project']}/{b['title']}"
        for r in rows:
            if not r["title"]:
                continue
            body_h = f"{board_hpath}/{r['title']}"
            body_id = sy.get_doc_id_by_path(nb, body_h)
            if not body_id:
                if not r["body_doc"]:
                    stats["no_body"] += 1
                    continue
                # экспорт тела из афины и импорт саб-доком доски
                try:
                    title, md = export_body(sid, WS, r["body_doc"])
                except Exception as e:
                    print("body export FAIL", r["title"][:30], str(e)[:60])
                    stats["fail"] += 1
                    continue
                try:
                    sy.create_doc_with_md(nb, body_h, md)
                    stats["bodies"] += 1
                    print("body:", body_h[:70])
                    time.sleep(0.4)
                except Exception as e:
                    print("body import FAIL", r["title"][:30], str(e)[:60])
                    stats["fail"] += 1
                    continue
            # комменты
            if not r["body_doc"]:
                continue
            body_id = sy.get_doc_id_by_path(nb, body_h)
            if not body_id:
                continue
            chk = sy.sql(f"select count(*) as n from blocks where root_id='{body_id}' and content like '%Комменты (миграция из афины)%'")
            if chk and chk[0].get("n", 0) > 0:
                continue
            try:
                nodes = threads_for(sid, r["body_doc"])
            except Exception as e:
                print("threads FAIL", r["title"][:30], str(e)[:60])
                continue
            if not nodes:
                continue
            try:
                sy._post("/api/block/appendBlock",
                         {"dataType": "markdown", "data": comment_md(nodes), "parentID": body_id})
                stats["comments"] += 1
                print("comments:", r["title"][:40])
            except Exception as e:
                print("append FAIL", r["title"][:30], str(e)[:60])
    print("DONE", stats)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
