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
  create <тайтл> <файл.md> [--parent <docId>]   — создать док (опц. под родителя-ДОК, печатает docId;
                                                --parent валидируется ДО создания: organize-папка или
                                                несуществующий док → отказ без создания сироты)
  link <docId> --folder <folderId>              — прилинковать док в organize-папку (для свежесозданного
                                                дока; идемпотентен)
  org <docId> --to <folderId>                   — переместить СУЩЕСТВУЮЩИЙ organize-линк в другую папку
  move <docId> --parent <docId>                 — перенести под родителя (doc-в-док)
  append <docId> <файл.md>                      — дописать markdown в конец дока
  replace <docId> <файл.md> [--stub]            — заменить контент (--stub: без длиновой защиты)
  export <docId>                                — живой markdown дока в stdout
  embed <docId> [--html файл] [--name id]        — вставить/заменить embed_html-диаграмму (replace по маркеру ⁂ diagram:<id>)
  orgls                                         — organize-папки (id + имя)
  children <folderId>                           — organize-дети папки (folder/doc-линки, порядок index)
  find <…>                                      — ВЫПИЛЕН: шлюз не отдаёт doc-дерево; docId по тайтлу — uwuwu-cli search
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


def _call_tool(sid: str, name: str, args: dict, timeout: int = 300) -> dict:
    """tools/call + валидация ответа. McpError ре-рейзится с префиксом имени
    инструмента — погибший шаг серии виден в логе обвязки. Ошибка handshake
    (initialize при пере-handshake) сюда не заворачивается и уходит без
    префикса — атрибуция места отказа честная. timeout=300: organize- и
    trash-операции шлюза ходят 240s+ — клиентский обрыв на дефолтных 120с
    при дожившем до успеха сервере плодит дубли при повторе."""
    try:
        r, _ = mcp_call(
            "tools/call", {"name": name, "arguments": args}, sid, timeout=timeout
        )
        return mcp_result(r)
    except McpError as e:
        raise McpError(f"{name}: {e}") from e


def call(sid: str, name: str, args: dict) -> dict:
    """tools/call с восстановлением протухшей сессии: 400/404 → один
    пере-handshake → повтор. Обновлённый sid живёт в модуле — последующие
    call(старый_sid,...) продолжают работать. Ошибки контракта приходят с
    префиксом имени инструмента (см. _call_tool). Таймаут 300с: organize-
    операции шлюза ходят 240s+; ТАЙМАУТ ≠ ПРОВАЛ — сервер мог дожить до
    успеха, перед повтором мутации проверь результат (orgls/search), иначе
    дубли."""
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


def _organize_nodes(sid: str) -> list[dict]:
    """Список organize-нод воркспейса ({id, parentId, type: folder|doc,
    data: имя папки | docId-цель линка, index}). Единственный листинг,
    отражающий organize-структуру: list_workspace_tree шлюза возвращает
    те же organize-ноды (без docId/title — doc-дерева в транспорте нет)."""
    res = call(sid, "list_organize_nodes", {"workspaceId": WORKSPACE})
    return json.loads(_content_text(res) or "{}")["nodes"]


def _content_text(res: object) -> str | None:
    """Первый content[].text из MCP-ответа (обёртки шлюза кладут JSON-полезную
    нагрузку именно там)."""
    if not isinstance(res, dict):
        return None
    for c in res.get("content", []):
        if isinstance(c, dict) and "text" in c:
            return c["text"]
    return None


def _validate_parent(sid: str, parent: str) -> int | None:
    """Валидация --parent ДО создания дока: вернуть exit-код отказа (0/None =
    ок) либо напечатать причину. Организе-папка по node.id → подсказка link;
    несуществующий docId шлюз помечает exists:false в УСПЕШНОМ ответе
    read_doc (isError нет!); отказ шлюза (McpError — в т.ч. флейк
    «space:load-doc timeout») не считается «не существует» — печатаем обе
    интерпретации; HTTP 503/сокет-таймаут (URLError) пролетают как
    транзиентные — сироты нет в любом исходе."""
    nodes = _organize_nodes(sid)
    if any(n.get("type") == "folder" and n.get("id") == parent for n in nodes):
        print(
            f"--parent {parent} — organize-ПАПКА (folderId), не docId: "
            "вложенность док-в-док так не строится. Создай без --parent, "
            f"затем: link <docId> --folder {parent}",
            file=sys.stderr,
        )
        return 1
    try:
        res = call(sid, "read_doc", {"workspaceId": WORKSPACE, "docId": parent})
    except McpError as e:  # noqa: S5754 — перехват осознанный: отказ read_doc классифицируем и печатаем подсказку, не пробрасываем traceback в CLI-обвязки
        print(
            "read_doc --parent упал с отказом шлюза: parent-док не существует "
            f"ЛИБО шлюз флейкнул — проверь и повтори (отказ: {str(e)[:200]})",
            file=sys.stderr,
        )
        return 1
    if isinstance(res, dict) and res.get("exists") is False:
        print(
            f"parent-док {parent} не существует (read_doc: exists:false)",
            file=sys.stderr,
        )
        return 1
    return None


def cmd_create(a) -> int:
    md = Path(a.file).read_text(encoding="utf-8")
    sid = session()
    try:
        if a.parent:
            veto = _validate_parent(sid, a.parent)
            if veto:
                return veto
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
        print(did)  # до move: падение move не оставит docId неизвестным
        if a.parent:
            try:
                call(
                    sid,
                    "move_doc",
                    {"workspaceId": WORKSPACE, "docId": did, "toParentDocId": a.parent},
                )
            except McpError as e:  # noqa: S5754 — частичный успех: docId уже напечатан, печатаем рецепт восстановления вместо traceback
                print(
                    f"doc {did} СОЗДАН, но move под {a.parent} не прошёл "
                    f"({str(e)[:200]}); проверь и повтори: move {did} "
                    f"--parent {a.parent}",
                    file=sys.stderr,
                )
                return 2
            except urllib.error.URLError as e:  # noqa: S5754 — тот же частичный успех для транзиентов (HTTP 503/сокет-таймаут, сервер мог дожить до успеха)
                print(
                    f"doc {did} СОЗДАН, но move под {a.parent} оборван по сети "
                    f"({str(e)[:120]}); ТАЙМАУТ ≠ ПРОВАЛ — проверь position "
                    f"(export {did}) перед повтором: move {did} --parent {a.parent}",
                    file=sys.stderr,
                )
                return 2
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
        for n in _organize_nodes(sid):
            if n.get("type") == "folder":
                print(n["id"], n.get("data"))
        return 0
    finally:
        close(sid)


def cmd_org(a) -> int:
    """Перенести doc-линк в organize-папку: org <docId> --to <folderId>.
    Перемещает ТОЛЬКО существующий линк — у свежесозданного дока линка нет:
    сначала link <docId> --folder <folderId>."""
    sid = session()
    try:
        nodes = _organize_nodes(sid)
        link = next(
            (n for n in nodes if n.get("type") == "doc" and n.get("data") == a.docId),
            None,
        )
        if not link:
            print(
                "линк дока не найден в organize — для свежесозданного дока "
                f"сначала: link {a.docId} --folder <folderId>",
                file=sys.stderr,
            )
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


def cmd_link(a) -> int:
    """Прилинковать док в organize-папку: link <docId> --folder <folderId>
    (обёртка gateway add_organize_link — единственный способ организовать
    свежесозданный док, у которого линка в organize ещё нет).
    Идемпотентен (инвариант шлюза: один линк на док — вердикты ниже
    опираются на первый найденный линк): линк уже в этой папке —
    «уже здесь», exit 0; в другой — предупреждение «нужен org», exit 1."""
    sid = session()
    try:
        nodes = _organize_nodes(sid)
        if not any(n.get("type") == "folder" and n.get("id") == a.folder for n in nodes):
            print(f"папка {a.folder} не найдена в organize (см. orgls)", file=sys.stderr)
            return 1
        try:
            target = call(
                sid, "read_doc", {"workspaceId": WORKSPACE, "docId": a.docId}
            )
        except McpError as e:  # noqa: S5754 — отказ до мутации: линк не создаём
            print(
                f"read_doc {a.docId} упал ({str(e)[:150]}) — линк не создаём: "
                "проверь docId и повтори",
                file=sys.stderr,
            )
            return 1
        if isinstance(target, dict) and target.get("exists") is False:
            print(
                f"док {a.docId} не существует (read_doc: exists:false) — "
                "висячий линк не создаём",
                file=sys.stderr,
            )
            return 1
        existing = next(
            (n for n in nodes if n.get("type") == "doc" and n.get("data") == a.docId),
            None,
        )
        if existing:
            if existing.get("parentId") == a.folder:
                print(f"уже здесь (линк в {a.folder})")
                return 0
            print(
                f"линк уже существует в {existing.get('parentId')} — "
                f"перемещение: org {a.docId} --to {a.folder}",
                file=sys.stderr,
            )
            return 1
        call(
            sid,
            "add_organize_link",
            {
                "workspaceId": WORKSPACE,
                "folderId": a.folder,
                "type": "doc",
                "targetId": a.docId,
            },
        )
        print("linked")
        return 0
    finally:
        close(sid)


def cmd_find(_a) -> int:
    """ВЫПИЛЕН: list_workspace_tree шлюза возвращает organize-ноды
    ({id, parentId, type, data, index}) — полей docId/title в ответе нет,
    поиск по тайтлу в этом транспорте невозможен в принципе (живой тест
    02.11.2026: 353KB ответа, 0 docId, 0 title). Заглушка ради честной
    миграции вызовов: docId по тайтлу ищет uwuwu-cli search (Manticore)."""
    print(
        "find выпилен: list_workspace_tree шлюза не содержит docId/title "
        "(только organize-ноды). docId по тайтлу: uwuwu-cli search \"<запрос>\" "
        "(серверный полнотекст, env UWUWU_AFFINE_UI_URL/EMAIL/PASSWORD)",
        file=sys.stderr,
    )
    return 1


def cmd_children(a) -> int:
    """Организе-дети папки: children <folderId> (подпапки + doc-линки,
    порядок index, маркер «# mode: organize»). Doc-дети (док-в-док)
    НЕ поддерживаются: list_workspace_tree шлюза отдаёт organize-ноды
    без docId/title — doc-дерево в этом транспорте недоступно."""
    sid = session()
    try:
        nodes = _organize_nodes(sid)
        if not any(n.get("type") == "folder" and n.get("id") == a.id for n in nodes):
            print(
                f"{a.id} — не organize-папка (см. orgls); doc-дети недоступны: "
                "шлюз не отдаёт doc-дерево (list_workspace_tree = organize-ноды)",
                file=sys.stderr,
            )
            return 1
        print(f"# mode: organize {a.id}")
        kids = sorted(
            (n for n in nodes if n.get("parentId") == a.id),
            key=lambda n: n.get("index") or 0,  # index может отсутствовать или быть null
        )
        for n in kids:
            if n.get("type") == "folder":
                print("folder", n["id"], n.get("data"))
            else:
                print("doc", n.get("data"))
        return 0
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
    sub.add_parser("orgls")
    emb = sub.add_parser("embed")
    emb.add_argument("docId")
    emb.add_argument("--html")
    emb.add_argument("--name")
    om = sub.add_parser("org")
    om.add_argument("docId")
    om.add_argument("--to", required=True)
    lk = sub.add_parser("link")
    lk.add_argument("docId")
    lk.add_argument("--folder", required=True)
    ch = sub.add_parser("children")
    ch.add_argument("id")
    a = p.parse_args()
    try:
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
            "link": cmd_link,
        }[a.cmd](a)
    except McpError as e:
        # контракт docstring McpError: exit-код — забота CLI-точки входа;
        # 2 = отказ шлюза (в отличие от 1 — наших валидационных отказов до мутаций)
        print(f"!! {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except McpError as e:
        print(f"!! {e}", file=sys.stderr)
        sys.exit(2)
