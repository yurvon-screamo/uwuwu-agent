"""Адресный донёс комментов: uwuwu-доска (тела по hpath, идемпотентно).

Читает строки «Задачи uwuwu»/«Кроны uwuwu» из афины, для задач с телами и
тредами аппендит «## Комменты (миграция из афины)» в саб-док тела в SiYuan.
"""
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import affine_source as wiki_ops
from siyuan_migrate.boards import (WS, build_node_to_doc, comment_md, parse_row,
                                   read_board_rows, threads_for)
from siyuan_migrate.siyuan_client import SiYuan

WS_ID = WS


def main() -> int:
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    nb = sy.ensure_notebook("Projects")
    sid = wiki_ops.session("siyuan-migrate-comments")

    mf = Path.home() / "siyuan-migration-work" / "export" / "manifest.json"
    import json
    manifest = json.loads(mf.read_text())
    n2d = build_node_to_doc()
    doc_titles = {}
    body_hpaths = {}
    for node, info in manifest["docs"].items():
        if not info.get("ok"):
            continue
        real = n2d.get(node, node)
        doc_titles[real] = Path(info["path"]).stem
        # hpath тела в SiYuan: путь манифеста relative_to ноутбука
        parts = Path(info["path"]).parts  # ('Projects', '<proj>', ..., 'x.md')
        if parts[0] in ("Projects", "Experience", "Daily"):
            body_hpaths[real] = "/" + "/".join(parts[1:-1])

    for board_title, board_hpath in [("Задачи uwuwu", "/uwuwu/Задачи uwuwu"),
                                     ("Кроны uwuwu", "/uwuwu/Кроны uwuwu")]:
        doc_id = next((n2d.get(n, n) for n, i in manifest["docs"].items()
                       if i.get("ok") and i["path"].endswith(board_title + ".md")), None)
        if not doc_id:
            print("board doc id not found for", board_title)
            continue
        rows = [parse_row(r, doc_titles) for r in read_board_rows(sid, doc_id)]
        moved = skipped = 0
        for r in rows:
            if not r["title"] or not r["body_doc"]:
                continue
            # тело в дереве проектов: его собственный hpath из манифеста
            body_h = body_hpaths.get(r["body_doc"])
            if not body_h:
                continue
            body_id = sy.get_doc_id_by_path(nb, body_h)
            if not body_id:
                continue
            # идемпотентность
            chk = sy.sql(f"select count(*) as n from blocks where root_id='{body_id}' and content like '%Комменты (миграция из афины)%'")
            if chk and chk[0].get("n", 0) > 0:
                skipped += 1
                continue
            try:
                nodes = threads_for(sid, r["body_doc"])
            except Exception as e:
                print("threads FAIL", r["title"][:30], str(e)[:80])
                continue
            if not nodes:
                continue
            md = comment_md(nodes)
            try:
                sy._post("/api/block/appendBlock",
                         {"dataType": "markdown", "data": md, "parentID": body_id})
                moved += 1
                print("moved:", r["title"][:40])
            except Exception as e:
                print("append FAIL", r["title"][:30], str(e)[:80])
            time.sleep(0.3)
        print(f"{board_title}: comments moved={moved} skipped={skipped}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
