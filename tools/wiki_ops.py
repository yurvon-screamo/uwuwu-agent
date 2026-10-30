#!/usr/bin/env python3
"""wiki_ops — CLI-хелпер MCP-операций над доками AFFiNE (для сабагентов).

Одна сессия на вызов, DELETE-закрытие в finally, 503-ретраи с паузами.
Гоча argparse: docId с ВЕДУЩИМ дефисом (напр. -GnAmdAk45) парсится как флаг —
опции ставь ДО `--`: wiki_ops.py move --parent <id> -- -GnAmdAk45.

Команды:
  create <тайтл> <файл.md> [--parent <docId>]   — создать док (опц. сразу под родителя), печатает docId
  move <docId> --parent <docId>                  — перенести под родителя
  append <docId> <файл.md>                       — дописать markdown в конец дока
  replace <docId> <файл.md> [--stub]             — заменить контент (--stub: без длиновой защиты)
  export <docId>                                 — живой markdown дока в stdout
  find <подстрока тайтла>                        — docId по тайтлу (живой поиск по дереву)
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from triage_affine import MCP_TOKEN, MCP_URL, WORKSPACE, get_md, mcp_call, mcp_result

RETRIES = 8
PAUSE = 120


def session() -> str:
    """Сессия с СЕЛЕКТИВНЫМИ ретраями: повторяем только транзиентное
    (HTTP 503, URLError/timeout); фатальные SystemExit от mcp_result
    (isError/JSON-RPC error) — сразу наверх с причиной."""
    for attempt in range(RETRIES):
        sid = None
        try:
            resp, sid = mcp_call(
                "initialize",
                {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "wiki-ops", "version": "1"},
                },
            )
            mcp_result(resp)  # SystemExit здесь = фатально, не ретраим
            mcp_call("notifications/initialized", {}, sid, notify=True)
            return sid
        except SystemExit:
            if sid:
                close(sid)
            raise
        except urllib.error.HTTPError as e:
            if sid:
                close(sid)
            if e.code != 503:
                raise
            _retry_wait(attempt, e)
        except urllib.error.URLError as e:
            if sid:
                close(sid)
            _retry_wait(attempt, e)
        # успешный путь: sid возвращается живым — close только в НЕУСПЕШНЫХ ветках
        # (finally закрыл бы сессию и при return — call после session ловил 404)
    raise RuntimeError("unreachable")


def _retry_wait(attempt: int, e: Exception) -> None:
    print(
        f"шлюз занят/недоступен ({attempt + 1}/{RETRIES}): {str(e)[:60]}; пауза {PAUSE}с",
        file=sys.stderr,
        flush=True,
    )
    time.sleep(PAUSE)


def close(sid: str) -> None:
    try:
        headers = {"mcp-session-id": sid}
        if MCP_TOKEN:
            headers["Authorization"] = f"Bearer {MCP_TOKEN}"
        urllib.request.urlopen(
            urllib.request.Request(MCP_URL, data=b"", method="DELETE", headers=headers),
            timeout=15,
        ).read()
    except urllib.error.URLError:
        pass


def call(sid: str, name: str, args: dict) -> dict:
    r, _ = mcp_call("tools/call", {"name": name, "arguments": args}, sid)
    return mcp_result(r)


def deep_md(o):
    """markdown из export-ответа (JSON-строка в content[0].text ЛИБО structuredContent)."""
    if isinstance(o, dict):
        if "markdown" in o and isinstance(o["markdown"], str):
            return o["markdown"]
        for c in o.get("content", []):
            if isinstance(c, dict) and isinstance(c.get("text"), str):
                try:
                    inner = json.loads(c["text"])
                except json.JSONDecodeError:
                    continue
                if isinstance(inner, dict) and isinstance(inner.get("markdown"), str):
                    return inner["markdown"]
        for v in o.values():
            r = deep_md(v)
            if r:
                return r
    elif isinstance(o, list):
        for i in o:
            r = deep_md(i)
            if r:
                return r
    return None


def cmd_create(a) -> int:
    md = Path(a.file).read_text(encoding="utf-8")
    sid = session()
    try:
        res = call(
            sid,
            "create_doc_from_markdown",
            {"workspaceId": WORKSPACE, "title": a.title, "markdown": md},
        )
        m = re.search(r'"docId"\s*:\s*"([^"]+)"', json.dumps(res))
        if not m:
            print("!! create не дал docId", file=sys.stderr)
            return 2
        did = m.group(1)
        if a.parent:
            call(
                sid,
                "move_doc",
                {"workspaceId": WORKSPACE, "docId": did, "toParentDocId": a.parent},
            )
        print(did)
        return 0
    finally:
        close(sid)


def cmd_move(a) -> int:
    sid = session()
    try:
        call(
            sid,
            "move_doc",
            {"workspaceId": WORKSPACE, "docId": a.docId, "toParentDocId": a.parent},
        )
        print("moved")
        return 0
    finally:
        close(sid)


def cmd_append(a) -> int:
    md = Path(a.file).read_text(encoding="utf-8")
    sid = session()
    try:
        call(
            sid,
            "append_markdown",
            {"workspaceId": WORKSPACE, "docId": a.docId, "markdown": md},
        )
        print("appended")
        return 0
    finally:
        close(sid)


def cmd_replace(a) -> int:
    new = Path(a.file).read_text(encoding="utf-8")
    sid = session()
    try:
        live = (
            get_md(
                call(
                    sid,
                    "export_doc_markdown",
                    {"workspaceId": WORKSPACE, "docId": a.docId},
                )
            )
            or ""
        )
        if live and not a.stub and len(new) < len(live) * 0.55:
            print(
                f"!! подозрительно коротко: {len(new)} vs {len(live)} (—stub чтобы разрешить)",
                file=sys.stderr,
            )
            return 2
        call(
            sid,
            "replace_doc_with_markdown",
            {"workspaceId": WORKSPACE, "docId": a.docId, "markdown": new},
        )
        print("replaced")
        return 0
    finally:
        close(sid)


def cmd_export(a) -> int:
    sid = session()
    try:
        md = get_md(
            call(
                sid, "export_doc_markdown", {"workspaceId": WORKSPACE, "docId": a.docId}
            )
        )
        print(md or "")
        return 0
    finally:
        close(sid)


def cmd_embed(a) -> int:
    """Вставить/заменить HTML-диаграмму (embed_html) в ЛЮБОЙ док.
    embed <docId> --html файл [--name arch] — с именем: replace-семантика по маркеру
    (без имени — простая вставка в конец).
    """
    html = Path(a.html).read_text(encoding="utf-8") if a.html else sys.stdin.read()
    sid = session()
    try:
        if a.name:
            marker = f"⁂ diagram:{a.name}"
            res = call(sid, "read_doc", {"workspaceId": WORKSPACE, "docId": a.docId})
            blocks = res.get("blocks", []) if isinstance(res, dict) else []
            note = next(
                (
                    b
                    for b in blocks
                    if b.get("flavour") == "affine:note"
                    or "note" in str(b.get("flavour", ""))
                ),
                {},
            )
            child_ids = [c for c in (note.get("childIds") or []) if isinstance(c, str)]
            by_id = {b.get("id"): b for b in blocks if isinstance(b, dict)}
            marker_id = next(
                (cid for cid in child_ids if by_id.get(cid, {}).get("text") == marker),
                None,
            )
            if marker_id:
                # replace: удалить следящий embed, вставить новый после маркера
                i = child_ids.index(marker_id)
                if i + 1 < len(child_ids):
                    nxt = child_ids[i + 1]
                    if "embed" in str(by_id.get(nxt, {}).get("flavour", "")):
                        call(
                            sid,
                            "delete_block",
                            {
                                "workspaceId": WORKSPACE,
                                "docId": a.docId,
                                "blockId": nxt,
                            },
                        )
                call(
                    sid,
                    "append_block",
                    {
                        "workspaceId": WORKSPACE,
                        "docId": a.docId,
                        "type": "embed_html",
                        "html": html,
                        "placement": {"afterBlockId": marker_id},
                    },
                )
                print(f"replaced diagram:{a.name}")
            else:
                r = call(
                    sid,
                    "append_block",
                    {
                        "workspaceId": WORKSPACE,
                        "docId": a.docId,
                        "type": "paragraph",
                        "text": marker,
                    },
                )
                mid = r.get("blockId") if isinstance(r, dict) else None
                args = {
                    "workspaceId": WORKSPACE,
                    "docId": a.docId,
                    "type": "embed_html",
                    "html": html,
                }
                if mid:
                    args["placement"] = {"afterBlockId": mid}
                call(sid, "append_block", args)
                print(f"inserted diagram:{a.name}")
        else:
            call(
                sid,
                "append_block",
                {
                    "workspaceId": WORKSPACE,
                    "docId": a.docId,
                    "type": "embed_html",
                    "html": html,
                },
            )
            print("inserted (anonymous)")
        return 0
    finally:
        close(sid)


def cmd_orgls(a) -> int:
    """Список organize-папок (id + имя + parent) — для поиска folderId."""
    sid = session()
    try:
        res = call(sid, "list_organize_nodes", {"workspaceId": WORKSPACE})

        def get_text(o):
            if isinstance(o, dict):
                for c in o.get("content", []):
                    if isinstance(c, dict) and "text" in c:
                        return c["text"]

        nodes = json.loads(get_text(res) or "{}")["nodes"]
        for n in nodes:
            if n.get("type") == "folder":
                print(n["id"], n.get("data"))
        return 0
    finally:
        close(sid)


def cmd_org(a) -> int:
    """Перенести doc-линк в organize-папку: org <docId> --to <folderId>."""
    sid = session()
    try:
        res = call(sid, "list_organize_nodes", {"workspaceId": WORKSPACE})

        def get_text(o):
            if isinstance(o, dict):
                for c in o.get("content", []):
                    if isinstance(c, dict) and "text" in c:
                        return c["text"]

        nodes = json.loads(get_text(res) or "{}")["nodes"]
        link = next(
            (n for n in nodes if n.get("type") == "doc" and n.get("data") == a.docId),
            None,
        )
        if not link:
            print("линк дока не найден в organize", file=sys.stderr)
            return 1
        call(
            sid,
            "move_organize_node",
            {"workspaceId": WORKSPACE, "nodeId": link["id"], "parentId": a.to},
        )
        print("moved")
        return 0
    finally:
        close(sid)


def cmd_children(a) -> int:
    sid = session()
    try:
        res = call(sid, "list_workspace_tree", {"workspaceId": WORKSPACE, "depth": 4})

        def get_text(o):
            if isinstance(o, dict):
                for c in o.get("content", []):
                    if isinstance(c, dict) and "text" in c:
                        return c["text"]

        tree = json.loads(get_text(res) or "{}")

        def node(n, d):
            if n["docId"] == d:
                return n
            for ch in n.get("children", []):
                r = node(ch, d)
                if r:
                    return r

        for root in tree.get("tree", []):
            nd = node(root, a.docId)
            if nd is not None:
                for c in nd.get("children", []):
                    t = c.get("title") or ""
                    print(
                        c["docId"],
                        json.loads(f'"{t}"') if "\\u" in t else t,
                    )
                return 0
        print("не найден", file=sys.stderr)
        return 1
    finally:
        close(sid)


def cmd_find(a) -> int:
    sid = session()
    try:
        res = call(sid, "list_workspace_tree", {"workspaceId": WORKSPACE, "depth": 4})
        s = json.dumps(res)
        pat = re.compile(
            r'"docId"\s*:\s*"([^"]+)"[^}]{0,300}?"title"\s*:\s*"((?:[^"\\]|\\.)*'
            + re.escape(a.query)
            + r'(?:[^"\\]|\\.)*)"',
            re.IGNORECASE,
        )
        hits = pat.findall(s)
        if not hits:  # порядок полей наоборот
            hits = [
                (d, t)
                for t, d in re.findall(
                    r'"title"\s*:\s*"((?:[^"\\]|\\.)*'
                    + re.escape(a.query)
                    + r'(?:[^"\\]|\\.)*)"[^}]{0,300}?"docId"\s*:\s*"([^"]+)"',
                    s,
                    re.IGNORECASE,
                )
            ]
        for did, t in hits[:10]:
            print(did, json.loads(f'"{t}"') if "\\u" in t else t)
        return 0 if hits else 1
    finally:
        close(sid)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("create")
    c.add_argument("title")
    c.add_argument("file")
    c.add_argument("--parent")
    m = sub.add_parser("move")
    m.add_argument("docId")
    m.add_argument("--parent", required=True)
    ap = sub.add_parser("append")
    ap.add_argument("docId")
    ap.add_argument("file")
    r = sub.add_parser("replace")
    r.add_argument("docId")
    r.add_argument("file")
    r.add_argument("--stub", action="store_true")
    e = sub.add_parser("export")
    e.add_argument("docId")
    f = sub.add_parser("find")
    f.add_argument("query")
    ch = sub.add_parser("children")
    ch.add_argument("docId")
    sub.add_parser("orgls")
    emb = sub.add_parser("embed")
    emb.add_argument("docId")
    emb.add_argument("--html")
    emb.add_argument("--name")
    om = sub.add_parser("org")
    om.add_argument("docId")
    om.add_argument("--to", required=True)
    a = p.parse_args()
    return {
        "create": cmd_create,
        "move": cmd_move,
        "append": cmd_append,
        "replace": cmd_replace,
        "export": cmd_export,
        "find": cmd_find,
        "children": cmd_children,
        "orgls": cmd_orgls,
        "embed": cmd_embed,
        "org": cmd_org,
    }[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
