"""RECOVERY: полный добор строк канбана из афины.

Для каждой строки каждой доски: реальный титул берётся ИЗ САМОГО ТЕЛА
(export_doc_markdown отвечает title), не из манифеста. Строка+тело+статус+
комменты досоздаются в SiYuan идемпотентно (по тайтлу).
"""
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import affine_source as wiki_ops
from siyuan_migrate.boards import (WS, SiYuanMcp, build_node_to_doc, comment_md,
                                   parse_row, read_board_rows, threads_for)
from siyuan_migrate.siyuan_client import SiYuan

WS_ID = WS


def is_pr_link(pr: str) -> bool:
    """PR-ячейка — ключ типа url: принимает только http(s)-ссылки (как cli edit --pr)."""
    return pr.startswith(("http://", "https://"))


def path_safe(t: str) -> str:
    return t.replace("/", "∕")


def export_body(sid, doc_id, attempts=3):
    last = "?"
    for _ in range(attempts):
        r = wiki_ops.call(sid, "export_doc_markdown", {"workspaceId": WS_ID, "docId": doc_id})
        for c in r.get("content", []):
            try:
                p = json.loads(c.get("text", ""))
            except Exception:
                continue
            if p.get("exists") is False:
                return None, None
            if p.get("exists") and (p.get("markdown") or "").strip():
                return str(p.get("title") or doc_id), p["markdown"]
            last = "empty"
        time.sleep(2)
    raise RuntimeError(f"export body failed: {last}")


def av_of_doc(sy, nb, doc_id):
    rows = sy.sql(f"select id, markdown from blocks where root_id='{doc_id}' and type='av'")
    for r in rows:
        m = re.search(r'data-av-id="([0-9]{14}-[a-z0-9]+)"', r.get("markdown") or "")
        if m:
            return m.group(1)
    return ""


def keys_of(mcp, av):
    v = mcp.tool("database", {"action": "keys", "id": av})
    arr = (v.get("data", {}) or {}).get("keys") or v.get("keys") or []
    return {k["name"]: k["id"] for k in arr if k.get("name")}


def existing_titles(mcp, av):
    try:
        v = mcp.tool("database", {"action": "render", "id": av})
        d = (v.get("data", {}) or {})
        cols = {c.get("id"): c.get("name") for c in (d.get("view", {}).get("columns") or [])}
        titles = set()
        for row in (d.get("view", {}).get("rows") or []):
            for c in row.get("cells", []):
                val = c.get("value", {})
                if val.get("type") == "block":
                    titles.add((val.get("block", {}) or {}).get("content", ""))
        return titles
    except Exception:
        return set()


def main():
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    mcp = SiYuanMcp("http://127.0.0.1:6806", sy.token)
    print("mcp:", mcp.initialize().get("serverInfo"))
    nb = sy.ensure_notebook("Projects")
    sid = wiki_ops.session("recovery")

    manifest = json.loads((Path.home() / "siyuan-migration-work" / "export" / "manifest.json").read_text())
    n2d = build_node_to_doc()
    doc_titles = {n2d.get(n, n): Path(i["path"]).stem for n, i in manifest["docs"].items() if i.get("ok")}

    # доски афины: docId реальный
    boards = []
    for n, i in manifest["docs"].items():
        if not i.get("ok"):
            continue
        name = Path(i["path"]).stem
        if name.startswith(("Задачи ", "Кроны ")):
            proj = Path(i["path"]).parts[-2]
            boards.append({"doc_id": n2d.get(n, n), "title": name, "project": proj})

    stats = {"rows_src": 0, "rows_added": 0, "bodies": 0, "comments": 0, "skipped": 0, "no_title": 0}
    for b in boards:
        try:
            raw = read_board_rows(sid, b["doc_id"])
        except Exception as e:
            print("READ FAIL", b["title"], str(e)[:80])
            continue
        board_h = f"/{b['project']}/{b['title']}"
        sy_doc = sy.get_doc_id_by_path(nb, board_h)
        if not sy_doc:
            print("нет доски в SiYuan:", board_h)
            continue
        av = av_of_doc(sy, nb, sy_doc)
        if not av:
            print("нет database:", board_h)
            continue
        keys = keys_of(mcp, av)
        done = existing_titles(mcp, av)
        for raw_row in raw:
            stats["rows_src"] += 1
            base = parse_row(raw_row, doc_titles)
            title = base["title"]
            body_doc = base["body_doc"]
            md = None
            if (not title or title in done) and body_doc:
                t2, md2 = export_body(sid, body_doc)
                if t2 and md2 is not None:
                    if not title:
                        title, md = t2.strip(), md2
            if not title:
                stats["no_title"] += 1
                continue
            if title in done:
                stats["skipped"] += 1
                continue
            # строка
            item = mcp.tool("database", {"action": "item_add", "id": av, "detached": True, "content": title})
            item_id = (item.get("data", {}) or {}).get("itemID", "")
            if not item_id:
                print("item_add FAIL", title[:30])
                continue
            stats["rows_added"] += 1
            time.sleep(0.3)
            # статус
            if base["status"] and keys.get("Status"):
                try:
                    mcp.tool("database", {"action": "item_update", "id": av, "keyID": keys["Status"],
                                             "itemID": item_id, "value": {"mSelect": [{"content": base["status"]}]}})
                except Exception as e:
                    print("status fail", str(e)[:60])
                time.sleep(0.2)
            for kname in ("Branch", "PR"):
                kval = base["branch"] if kname == "Branch" else base["pr"]
                if not kval or not keys.get(kname):
                    continue
                if kname == "PR" and not is_pr_link(kval):
                    # PR-ячейка — ключ типа url: не-ссылка оставила бы осиротевшую ячейку
                    print(f"PR skip (ожидаю http(s)://…): {kval[:40]}")
                    continue
                value = {"text": {"content": kval}} if kname == "Branch" else {"url": {"content": kval}}
                try:
                    mcp.tool("database", {"action": "item_update", "id": av, "keyID": keys[kname],
                                             "itemID": item_id, "value": value})
                except Exception:
                    pass
                time.sleep(0.2)
            # тело (создание терпеливо: холодный индекс после рестарта контейнера)
            body_h = f"{board_h}/{path_safe(title)}"
            if not sy.get_doc_id_by_path(nb, body_h):
                if md is None and body_doc:
                    _, md = export_body(sid, body_doc)
                if md:
                    for attempt in range(8):
                        try:
                            sy.create_doc_with_md(nb, body_h, md)
                            stats["bodies"] += 1
                            break
                        except Exception as e:
                            if "metadata is not ready" in str(e) and attempt < 7:
                                time.sleep(3 + attempt * 3)
                                continue
                            print("body create FAIL", title[:30], str(e)[:70])
                    time.sleep(0.3)
            # комменты
            if body_doc:
                body_id = sy.get_doc_id_by_path(nb, body_h)
                if body_id:
                    chk = sy.sql(f"select count(*) as n from blocks where root_id='{body_id}' and content like '%Комменты (миграция из афины)%'")
                    if not (chk and chk[0].get("n", 0) > 0):
                        try:
                            nodes = threads_for(sid, body_doc)
                            if nodes:
                                sy._post("/api/block/appendBlock", {"dataType": "markdown", "data": comment_md(nodes), "parentID": body_id})
                                stats["comments"] += 1
                        except Exception as e:
                            print("comments fail", title[:25], str(e)[:60])
                    time.sleep(0.2)
            done.add(title)
        print(f"{b['title']}: src обработан (added={stats['rows_added']} на этой волне)")
    print("DONE", stats)


if __name__ == "__main__":
    main()
