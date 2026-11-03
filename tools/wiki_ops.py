#!/usr/bin/env python3
"""wiki_ops — CLI-хелпер MCP-операций над доками AFFiNE (для сабагентов).

Сессия переиспользуется через ~/.uwuwu/mcp-session.txt (общая с uwuwu-cli):
initialize = sign-in афины 4-7с, платим только когда сохранённая протухла —
call() тогда сам пере-handshake'ится (400/404 → один ретрай). 503-ретраи
транзиентов с паузами сохранены; close() в finally трогает только
осиротевшие сессии — общую не инвалидирует.
Гоча argparse: docId с ВЕДУЩИМ дефисом (напр. -GnAmdAk45) парсится как флаг —
опции ставь ДО `--`: wiki_ops.py move --parent <id> -- -GnAmdAk45.

Команды:
  create <тайтл> <файл.md> [--parent <docId>]   — создать док (опц. сразу под родителя), печатает docId
  move <docId> --parent <docId>                  — перенести под родителя
  append <docId> <файл.md>                       — дописать markdown в конец дока
  replace <docId> <файл.md> [--stub]             — заменить контент (--stub: без длиновой защиты)
  export <docId>                                 — живой markdown дока в stdout
  find <подстрока тайтла>                        — docId по тайтлу (живой поиск по дереву)
  unlink <docId>                                 — снять organize-линк(и) дока из папок (дедуп: убирает из сайдбара; из поиска док уходит только в корзину)
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
from triage_affine import (
    MCP_TOKEN,
    MCP_URL,
    McpError,
    WORKSPACE,
    get_md,
    mcp_call,
    mcp_result,
)

RETRIES = 8
PAUSE = 120

# Общая сессия с uwuwu-cli (Rust): initialize = sign-in афины 4-7с на каждый
# запуск — главный источник латентности python-тулз. Файл разделяемый;
# протухшую сессию call() пере-handshake'ит сам (паттерн affine_client.rs).
SESSION_FILE = Path.home() / ".uwuwu" / "mcp-session.txt"
_live_sid: str | None = None


def _load_saved_session() -> str:
    try:
        return SESSION_FILE.read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def _save_session(sid: str) -> None:
    try:
        SESSION_FILE.parent.mkdir(parents=True, exist_ok=True)
        SESSION_FILE.write_text(sid, encoding="utf-8")
    except OSError:
        pass  # сохранить не смогли — сессия живёт до idle-timeout шлюза


def _handshake(client_name: str = "wiki-ops") -> str:
    """Сессия с СЕЛЕКТИВНЫМИ ретраями: повторяем только транзиентное
    (HTTP 503, URLError/timeout); McpError от mcp_result
    (isError/JSON-RPC error) — фатально, сразу наверх с причиной.
    client_name виден на уровне ПРОТОКОЛА (initialize.clientInfo,
    MCP-трейс/инспектор); stdout шлюза логирует только UUID сессий."""
    for attempt in range(RETRIES):
        sid = None
        try:
            resp, sid = mcp_call(
                "initialize",
                {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": client_name, "version": "1"},
                },
            )
            mcp_result(resp)  # McpError здесь = фатально, не ретраим
            mcp_call("notifications/initialized", {}, sid, notify=True)
            _save_session(sid)
            return sid
        except McpError:
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


def session(client_name: str = "wiki-ops") -> str:
    """Переиспользовать сохранённую сессию (общая с uwuwu-cli), иначе
    полный handshake. Протухшая сессия ловится в call() и обновляется.
    client_name — идентичность создателя на уровне протокола initialize
    (MCP-трейс); в stdout-логах шлюза её нет, там только UUID."""
    if _live_sid:
        return _live_sid
    saved = _load_saved_session()
    if saved:
        return saved
    return _handshake(client_name)


def _retry_wait(attempt: int, e: Exception) -> None:
    print(
        f"шлюз занят/недоступен ({attempt + 1}/{RETRIES}): {str(e)[:60]}; пауза {PAUSE}с",
        file=sys.stderr,
        flush=True,
    )
    time.sleep(PAUSE)


def close(sid: str) -> None:
    """DELETE /mcp — только для осиротевших сессий (недоделанный handshake).
    Общую сохранённую сессию не закрываем: она разделяема с uwuwu-cli и
    следующими запусками — закрытие здесь инвалидировало бы файл для всех.
    Осиротевшие умрут по idle-timeout шлюза, но DELETE дешевле слота."""
    if sid and sid == _load_saved_session():
        return
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


def _call_tool(sid: str, name: str, args: dict) -> dict:
    """tools/call + валидация ответа. McpError ре-рейзится с префиксом имени
    инструмента — погибший шаг серии виден в логе обвязки. Ошибка handshake
    (initialize при пере-handshake) сюда не заворачивается и уходит без
    префикса — атрибуция места отказа честная."""
    try:
        r, _ = mcp_call("tools/call", {"name": name, "arguments": args}, sid)
        return mcp_result(r)
    except McpError as e:
        raise McpError(f"{name}: {e}") from e


def call(sid: str, name: str, args: dict) -> dict:
    """tools/call с восстановлением протухшей сессии: 400/404 → один
    пере-handshake → повтор. Обновлённый sid живёт в модуле — последующие
    call(старый_sid,...) продолжают работать. Ошибки контракта приходят с
    префиксом имени инструмента (см. _call_tool)."""
    global _live_sid
    effective = _live_sid or sid
    try:
        return _call_tool(effective, name, args)
    except urllib.error.HTTPError as e:
        if e.code not in (400, 404):
            raise
        e.close()  # отпустить соединение до повторного запроса
        print(
            "wiki-ops: сессия протухла — пере-handshake", file=sys.stderr, flush=True
        )
        _live_sid = _handshake()
        return _call_tool(_live_sid, name, args)


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


def _org_nodes(sid: str) -> list:
    """Ноды organize-дерева из list_organize_nodes. Контракт шлюза: JSON в
    content[0].text (общий для orgls/org/unlink — правится в одном месте).
    Неожиданная форма (нет/не список nodes) — McpError, не голый KeyError:
    форма ответа — часть контракта шлюза, как не-JSON в mcp_call."""
    res = call(sid, "list_organize_nodes", {"workspaceId": WORKSPACE})
    text = next(
        (
            c["text"]
            for c in (res.get("content", []) if isinstance(res, dict) else [])
            if isinstance(c, dict) and isinstance(c.get("text"), str)
        ),
        None,
    )
    data = json.loads(text) if text else {}
    nodes = data.get("nodes") if isinstance(data, dict) else None
    if not isinstance(nodes, list):
        raise McpError(f"list_organize_nodes: в ответе нет nodes: {str(res)[:200]}")
    return nodes


def cmd_orgls(a) -> int:
    """Список organize-папок (id + имя + parent) — для поиска folderId."""
    sid = session()
    try:
        nodes = _org_nodes(sid)
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
        nodes = _org_nodes(sid)
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


def cmd_unlink(a) -> int:
    """Снять organize-линк(и) дока: unlink <docId>. Док может быть слинкован
    в несколько папок — снимаются все его линки (дедуп-кейс: убираем из
    сайдбара копию, оставляя канон). Тело дока при этом живо и ищется."""
    sid = session()
    try:
        nodes = _org_nodes(sid)
        links = [
            n for n in nodes if n.get("type") == "doc" and n.get("data") == a.docId
        ]
        if not links:
            print("линк дока не найден в organize", file=sys.stderr)
            return 1
        for link in links:
            call(
                sid,
                "delete_organize_link",
                {"workspaceId": WORKSPACE, "nodeId": link["id"]},
            )
            print("unlinked", link["id"], "(parent", link.get("parentId"), ")")
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
    u = sub.add_parser("unlink")
    u.add_argument("docId")
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
        "unlink": cmd_unlink,
    }[a.cmd](a)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except McpError as e:
        print(f"!! {e}", file=sys.stderr)
        sys.exit(2)
