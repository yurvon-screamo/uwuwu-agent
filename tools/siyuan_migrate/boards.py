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
        sc = r.get("structuredContent", r)
        edges = (sc.get("edges") or [])
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

        self.idc += 1
        h = dict(self.headers)
        if self.sid:
            h["Mcp-Session-Id"] = self.sid
        req = urllib.request.Request(
            self.url, data=json.dumps({"jsonrpc": "2.0", "id": self.idc, "method": method, "params": params}).encode(),
            headers=h, method="POST",
        )
        with urllib.request.urlopen(req, timeout=120) as resp:
            if not self.sid:
                self.sid = resp.headers.get("mcp-session-id") or ""
            raw = resp.read().decode()
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


def find_boards(manifest: dict) -> list[dict]:
    boards = []
    for doc_id, info in manifest["docs"].items():
        if not info.get("ok"):
            continue
        name = Path(info["path"]).stem
        if name.startswith("Задачи ") or name.startswith("Кроны "):
            boards.append({"doc_id": doc_id, "title": name, "path": info["path"]})
    return boards


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


def parse_row(row: dict) -> dict:
    cells = row.get("cells") or {}
    def cell(k):
        c = cells.get(k) or {}
        v = c.get("value")
        return v if isinstance(v, str) else ""
    return {
        "row_id": row.get("rowBlockId") or row.get("rowId") or row.get("id") or "",
        "title": (row.get("title") or "").strip(),
        "body_doc": row.get("linkedDocId") or "",
        "status": cell("Status"),
        "pr": cell("PR"),
        "branch": cell("Branch"),
    }


def migrate_board(sy: SiYuanMcp, board: dict, rows: list[dict], target_nb: str) -> dict:
    """Create doc + database + kanban view in SiYuan."""
    # 1. carrier doc
    doc = sy.tool("document", {"action": "create", "notebook": target_nb, "parentID": "",
                               "title": board["title"], "content": f"# {board['title']}\n"})
    doc_id = doc.get("id") or ""
    if not doc_id:
        # док мог уже существовать — найдём по заголовку
        found = sy.tool("sql", {"action": "query",
                                "stmt": f"select id from blocks where type='d' and content='{board['title']}' limit 1"})
        rows_ = found.get("rows") or found.get("data") or []
        doc_id = (rows_[0].get("id") if rows_ else "") if isinstance(rows_, list) else ""
    stats = {"board": board["title"], "doc": doc_id, "rows": 0, "db": ""}
    if not doc_id:
        return stats
    time.sleep(1.5)
    # 2. database
    created = sy.tool("database", {"action": "create", "parentID": doc_id, "name": board["title"]})
    av = created.get("data", {}).get("avID") or created.get("avID") or ""
    stats["db"] = av
    if not av:
        return stats
    # 3. keys: Status (select) + Branch/PR (text)
    k_status = sy.tool("database", {"action": "key_add", "id": av, "name": "Status", "type": "select"})
    key_status = k_status.get("data", {}).get("key", {}).get("id") or k_status.get("key", {}).get("id") or ""
    sy.tool("database", {"action": "key_add", "id": av, "name": "Branch", "type": "text"})
    sy.tool("database", {"action": "key_add", "id": av, "name": "PR", "type": "text"})
    # опции статусов
    statuses = sorted({r["status"] for r in rows if r["status"]})
    if statuses and key_status:
        sy.tool("database", {"action": "key_update", "id": av, "keyID": key_status,
                             "config": {"options": [{"name": s} for s in statuses]}})
    # 4. rows
    for r in rows:
        if not r["title"]:
            continue
        item = sy.tool("database", {"action": "item_add", "id": av, "detached": True, "content": r["title"]})
        iid = (item.get("data", {}) or {}).get("itemID") or item.get("itemID") or ""
        if not iid:
            continue
        stats["rows"] += 1
        time.sleep(0.4)
        if r["status"] and key_status:
            sy.tool("database", {"action": "item_update", "id": av, "keyID": key_status, "itemID": iid,
                                 "value": {"select": {"content": r["status"]}}})
        if r["branch"]:
            kbr = sy.tool("database", {"action": "keys", "id": av})
            keys = (kbr.get("data", {}) or {}).get("keys") or kbr.get("keys") or []
            kb = next((k["id"] for k in keys if k.get("name") == "Branch"), "")
            if kb:
                sy.tool("database", {"action": "item_update", "id": av, "keyID": kb, "itemID": iid,
                                     "value": {"text": {"content": r["branch"]}}})
        time.sleep(0.4)
    # 5. kanban view
    if key_status:
        try:
            sy.tool("database", {"action": "view_add", "id": av, "blockID": doc_id,
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
    boards = find_boards(manifest)
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
    report = []
    for b in boards:
        rows = [parse_row(r) for r in read_board_rows(sid, b["doc_id"])]
        stats = migrate_board(sy_mcp, b, rows, nb_id)
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
