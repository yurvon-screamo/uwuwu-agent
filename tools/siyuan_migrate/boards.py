"""Stage 4: migrate AFFiNE kanban boards («Задачи <Proj>», «Кроны <Proj>») into SiYuan.

- Board rows → SiYuan database (Status select + Branch/PR text keys) + kanban view
- Task comments (threads+replies) → appended «Комменты (миграция)» section
  into the corresponding body doc in SiYuan (matched by title)

Uses: AFFiNE MCP gateway (wiki_ops) for source; SiYuan /mcp for databases.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import wiki_ops  # noqa: E402

WS = "81475705-028a-4153-a8f5-f06e402ee31e"


def ts(created: int) -> str:
    try:
        return dt.datetime.fromtimestamp(created / 1000, tz=dt.timezone.utc).strftime("%Y-%m-%d %H:%M")
    except Exception:  # noqa: BLE001
        return str(created)


def threads_for(sid: str, body_doc: str) -> list[dict]:
    out: list[dict] = []
    cursor = ""
    for _ in range(10):
        args = {"workspaceId": WS, "docId": body_doc, "first": 50}
        if cursor:
            args["after"] = cursor
        r = wiki_ops.call(sid, "list_comments", args)
        sc = r.get("structuredContent") if isinstance(r, dict) else None
        if sc is None and isinstance(r, dict):
            sc = r
        if not isinstance(sc, dict):
            sc = {}
        edges = sc.get("edges") or []
        for e in edges:
            node = e.get("node") or {}
            out.append(node)
        page = sc.get("pageInfo") or {}
        if not page.get("hasNextPage"):
            break
        cursor = page.get("endCursor") or ""
    return out


def comment_md(nodes: list[dict]) -> str:
    lines = ["", "## Комменты (миграция из афины)", ""]
    def text_of(content: dict) -> str:
        if isinstance(content, dict):
            body = content.get("body") or content.get("text") or ""
            if isinstance(body, list):
                return " ".join(str(x) for x in body)
            return str(body)
        return str(content or "")
    for node in sorted(nodes, key=lambda n: str((n.get("content") or {}).get("createDate", ""))):
        c = node.get("content") or {}
        who = (node.get("user") or {}).get("name") or (node.get("user") or {}).get("id") or "?"
        when = str(c.get("createDate") or "")[:16].replace("T", " ")
        body = text_of(c).strip()
        lines.append(f"- **{who}** ({when}): {body}")
        for rep in node.get("replies") or []:
            rwho = (rep.get("user") or {}).get("name") or (rep.get("user") or {}).get("id") or "?"
            rwhen = ts(rep.get("created") or 0)
            rbody = text_of(rep.get("content") or {}).strip()
            lines.append(f"  - **{rwho}** ({rwhen}): {rbody}")
        if node.get("resolved"):
            lines.append("  - *(тред резолвнут)*")
    return "\n".join(lines) + "\n"


class SiYuanMcp:
    """Thin MCP client over SiYuan /mcp (streamable HTTP)."""

    def __init__(self, url: str, token: str):
        self.url = url.rstrip("/") + "/mcp"
        self.headers = {
            "Authorization": f"Token {token}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        self.sid = ""
        self.idc = 0

    def _rpc(self, method: str, params: dict) -> dict:
        import urllib.request

        is_notification = method.startswith("notifications/")
        if not is_notification:
            self.idc += 1
        payload = {"jsonrpc": "2.0", "method": method, "params": params}
        if not is_notification:
            payload["id"] = self.idc
        h = dict(self.headers)
        if self.sid:
            h["Mcp-Session-Id"] = self.sid
        req = urllib.request.Request(
            self.url, data=json.dumps(payload).encode(),
            headers=h, method="POST",
        )
        with urllib.request.urlopen(req, timeout=120) as resp:
            if not self.sid:
                self.sid = resp.headers.get("mcp-session-id") or ""
            raw = resp.read().decode()
        if not raw.strip():
            return {}  # нотификации возвращают 202 с пустым телом
        # SSE: последняя строка data:
        data_lines = [ln[5:] for ln in raw.splitlines() if ln.startswith("data:")]
        payload = json.loads(data_lines[-1] if data_lines else raw)
        if "error" in payload:
            raise RuntimeError(f"MCP {method}: {payload['error']}")
        return payload.get("result", {})

    def initialize(self):
        r = self._rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                     "clientInfo": {"name": "uwuwu-migrate", "version": "1.0"}})
        self._rpc("notifications/initialized", {})
        return r

    def tool(self, name: str, arguments: dict):
        r = self._rpc("tools/call", {"name": name, "arguments": arguments})
        if r.get("isError"):
            raise RuntimeError(f"tool {name}: {r.get('content', [{}])[0].get('text', '')[:200]}")
        sc = r.get("structuredContent") or {}
        if "error" in sc:
            raise RuntimeError(f"tool {name}: {sc['error']}")
        text = (r.get("content") or [{}])[0].get("text", "")
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return text


def find_boards(manifest: dict, node_to_doc: dict | None = None) -> list[dict]:
    boards = []
    for node_id, info in manifest["docs"].items():
        if not info.get("ok"):
            continue
        name = Path(info["path"]).stem
        if name.startswith("Задачи ") or name.startswith("Кроны "):
            real_doc = (node_to_doc or {}).get(node_id, node_id)
            boards.append({"doc_id": real_doc, "title": name, "path": info["path"]})
    return boards


def build_node_to_doc() -> dict:
    """id строки дерева → настоящий docId (для доков data=docId)."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    import wiki_ops
    sid = wiki_ops.session("siyuan-migrate-boards")
    r = wiki_ops.call(sid, "list_organize_nodes", {"workspaceId": WS})
    nodes = r.get("structuredContent", r).get("nodes", [])
    return {n["id"]: str(n.get("data") or n["id"]) for n in nodes if n.get("type") == "doc"}


def read_board_rows(sid: str, board_doc: str) -> list[dict]:
    doc = wiki_ops.call(sid, "read_doc", {"workspaceId": WS, "docId": board_doc})
    blocks = doc.get("blocks") or doc.get("structuredContent", {}).get("blocks") or []
    db_id = next((b.get("id") for b in blocks
                  if b.get("flavour") in ("affine:database", "affine:data_view")), "")
    if not db_id:
        return []
    r = wiki_ops.call(sid, "read_database_cells",
                      {"workspaceId": WS, "docId": board_doc, "databaseBlockId": db_id})
    return r.get("rows") or r.get("structuredContent", {}).get("rows") or []


def parse_row(row: dict, doc_titles: dict | None = None) -> dict:
    cells = row.get("cells") or {}
    def cell(k):
        c = cells.get(k) or {}
        v = c.get("value")
        return v if isinstance(v, str) else ""
    title = (row.get("title") or "").strip()
    linked = row.get("linkedDocId") or ""
    if not title and linked and doc_titles:
        title = (doc_titles.get(linked) or "").strip()
    return {
        "row_id": row.get("rowBlockId") or row.get("rowId") or row.get("id") or "",
        "title": title,
        "body_doc": linked,
        "status": cell("Status"),
        "pr": cell("PR"),
        "branch": cell("Branch"),
    }


def migrate_board(sy: SiYuanMcp, board: dict, rows: list[dict], target_nb: str, sy_http=None) -> dict:
    """Database + kanban view внутри импортированного дока /{proj}/<title>."""
    # проект из «Задачи <proj>» / «Кроны <proj>»
    prefix = "Задачи " if board["title"].startswith("Задачи ") else "Кроны "
    proj = board["title"][len(prefix):]
    hpath = f"/{proj}/{board['title']}"
    doc_id = ""
    if sy_http is not None:
        try:
            found = sy_http.sql(
                f"select id from blocks where type='d' and hpath='{hpath}' limit 1")
            if found:
                doc_id = found[0]["id"]
        except Exception:
            pass
    if not doc_id:
        doc = sy.tool("document", {"action": "create", "notebook": target_nb, "parentID": "",
                                   "title": board["title"], "content": f"# {board['title']}\n"})
        if isinstance(doc, str):
            m = re.search(r"[0-9]{14}-[a-z0-9]+", doc)
            doc_id = m.group(0) if m else ""
        elif isinstance(doc, dict):
            doc_id = doc.get("id") or doc.get("data", {}).get("id") or ""
    stats = {"board": board["title"], "doc": doc_id, "rows": 0, "db": ""}
    if not doc_id:
        return stats
    time.sleep(2)  # дать индексу догнать
    # database уже встроена?
    av = ""
    carrier = ""
    if sy_http is not None:
        try:
            blocks = sy_http.sql(
                f"select id, markdown from blocks where root_id='{doc_id}' and type='i' and markdown like '%avID%' limit 5")
            for b in blocks or []:
                m = re.search(r'avID="([0-9]{14}-[a-z0-9]+)"', b.get("markdown", "") or "")
                if m:
                    av = m.group(1)
                    carrier = b.get("id", "")
                    break
        except Exception:
            pass
    if not av:
        created = sy.tool("database", {"action": "create", "parentID": doc_id, "name": board["title"]})
        data = created.get("data", created)
        av = data.get("avID") or created.get("avID") or ""
        carrier = data.get("blockID") or created.get("blockID") or ""
    stats["db"] = av
    if not av:
        return stats
    # существующие строки (идемпотентность)
    existing = set()
    try:
        rr = sy._rpc("tools/call", {"name": "database",
                                    "arguments": {"action": "render", "id": av, "blockID": carrier or doc_id}})
        for row in (rr.get("structuredContent", {}).get("data", {}).get("view", {}).get("rows", []) or []):
            for c in row.get("cells", []):
                v = c.get("value", {})
                if v.get("type") == "block":
                    existing.add((v.get("block", {}) or {}).get("content", ""))
    except Exception:
        pass
    # 3. keys: Status (select) + Branch/PR (text) — только для свежей database
    if not existing:
        k_status = sy.tool("database", {"action": "key_add", "id": av, "name": "Status", "type": "select"})
        key_status = k_status.get("data", {}).get("key", {}).get("id") or k_status.get("key", {}).get("id") or ""
        sy.tool("database", {"action": "key_add", "id": av, "name": "Branch", "type": "text"})
        sy.tool("database", {"action": "key_add", "id": av, "name": "PR", "type": "text"})
    else:
        # существующая database: найти ключ Status
        kbr = sy.tool("database", {"action": "keys", "id": av})
        keys = (kbr.get("data", {}) or {}).get("keys") or kbr.get("keys") or []
        key_status = next((k["id"] for k in keys if k.get("name") == "Status"), "")
    # опции статусов
    statuses = sorted({r["status"] for r in rows if r["status"]})
    if statuses and key_status:
        sy.tool("database", {"action": "key_update", "id": av, "keyID": key_status,
                             "config": {"options": [{"name": s} for s in statuses]}})
    # 4. rows (существующие скипаем — идемпотентность)
    for r in rows:
        if not r["title"] or r["title"] in existing:
            continue
        item = sy.tool("database", {"action": "item_add", "id": av, "detached": True, "content": r["title"]})
        iid = (item.get("data", {}) or {}).get("itemID") or item.get("itemID") or ""
        if not iid:
            continue
        stats["rows"] += 1
        time.sleep(0.4)
        if r["status"] and key_status:
            sy.tool("database", {"action": "item_update", "id": av, "keyID": key_status, "itemID": iid,
                                 "value": {"mSelect": [{"content": r["status"]}]}})
        if r["branch"]:
            kbr = sy.tool("database", {"action": "keys", "id": av})
            keys = (kbr.get("data", {}) or {}).get("keys") or kbr.get("keys") or []
            kb = next((k["id"] for k in keys if k.get("name") == "Branch"), "")
            if kb:
                sy.tool("database", {"action": "item_update", "id": av, "keyID": kb, "itemID": iid,
                                     "value": {"text": {"content": r["branch"]}}})
        time.sleep(0.4)
    # 5. kanban view (blockID = блок-носитель database, не док)
    if key_status:
        try:
            sy.tool("database", {"action": "view_add", "id": av, "blockID": carrier or doc_id,
                                 "name": "Канбан", "layout": "kanban"})
        except Exception as e:  # noqa: BLE001
            stats["view_err"] = str(e)[:120]
    return stats


def append_comments(sy_http, body_title: str, md: str) -> bool:
    """Find doc by title in SiYuan and append comments section."""
    rows = sy_http.sql(f"select id from blocks where type='d' and content = '{body_title.replace(chr(39), chr(39)*2)}' limit 3")
    if not rows:
        return False
    doc_id = rows[0]["id"]
    import urllib.request
    body = json.dumps({"dataType": "markdown", "data": md, "parentID": doc_id}).encode()
    req = urllib.request.Request("http://127.0.0.1:6806/api/block/appendBlock",
                                 data=body, method="POST")
    req.add_header("Authorization", f"Token {sy_http.token}")
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    return data.get("code") == 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default=str(Path.home() / "siyuan-migration-work" / "export" / "manifest.json"))
    ap.add_argument("--ws", default=str(Path.home() / "siyuan-demo-ws"))
    ap.add_argument("--url", default="http://127.0.0.1:6806")
    ap.add_argument("--nb", default="Projects", help="notebook name for boards")
    ap.add_argument("--comments", action="store_true", help="also migrate task comments into body docs")
    ap.add_argument("--only", default="", help="substring filter by board title")
    a = ap.parse_args()

    manifest = json.loads(Path(a.manifest).read_text())
    boards = find_boards(manifest, build_node_to_doc())
    if a.only:
        boards = [b for b in boards if a.only in b["title"]]
    print(f"boards found: {[b['title'] for b in boards]}")

    token = json.loads((Path(a.ws) / "conf" / "conf.json").read_text())["api"]["token"]
    sy_mcp = SiYuanMcp(a.url, token)
    print("mcp:", sy_mcp.initialize().get("serverInfo"))

    from .siyuan_client import SiYuan
    sy_http = SiYuan(a.url, token)
    nb_id = sy_http.ensure_notebook(a.nb)

    sid = wiki_ops.session("siyuan-migrate-boards")
    # docId → title (для линк-строк: title живёт в теле-доке)
    n2d = build_node_to_doc()
    doc_titles = {}
    for node, info in manifest["docs"].items():
        if info.get("ok"):
            doc_titles[n2d.get(node, node)] = Path(info["path"]).stem
    report = []
    for b in boards:
        rows = [parse_row(r, doc_titles) for r in read_board_rows(sid, b["doc_id"])]
        stats = migrate_board(sy_mcp, b, rows, nb_id, sy_http)
        if a.comments:
            moved = 0
            for r in rows:
                if not r["body_doc"]:
                    continue
                try:
                    nodes = threads_for(sid, r["body_doc"])
                except Exception as e:  # noqa: BLE001
                    print(f"comments read FAIL {r['title'][:30]}: {e}")
                    continue
                if not nodes:
                    continue
                try:
                    if append_comments(sy_http, r["title"], comment_md(nodes)):
                        moved += 1
                except Exception as e:  # noqa: BLE001
                    print(f"comments append FAIL {r['title'][:30]}: {e}")
                time.sleep(0.3)
            stats["comments_moved"] = moved
        report.append(stats)
        print(json.dumps(stats, ensure_ascii=False), flush=True)
    out = Path(a.manifest).parent / "boards_report.json"
    out.write_text(json.dumps(report, ensure_ascii=False, indent=1))
    print("report →", out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
