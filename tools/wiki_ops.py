#!/usr/bin/env python3
"""wiki_ops — CLI-хелпер MCP-операций над доками AFFiNE (для сабагентов).

Сессия: последовательные запуски шарят ~/.uwuwu/mcp-session.txt с uwuwu-cli
(initialize = sign-in афины 4-7с — платим только когда сохранённая протухла,
call() сам пере-handshake'ится: 400/404 → один ретрай). Эксклюзивность —
flock на ~/.uwuwu/mcp-session.lock: лок наш → шарим и публикуем handshake в
общий файл; лок чужой → private-сессия, файл не читаем и не пишем. JSON-RPC
id — случайная база + счётчик (общая сессия с коллизией id давала кросс-ток
ответов, задача IyzxmFdujA). call() ретраит транзиентные сбои шлюза
(429/5xx/сеть; ok:false+retryable — чтению всегда, записи только кодам
_WRITE_TRUSTED_CODES, ADR-006) с экспоненциальным бэкофом; таймаут на
записи НЕ ретраится — операция могла примениться. close() в finally трогает
только осиротевшие сессии — общую не инвалидирует.
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
import atexit
import fcntl
import json
import os
import re
import socket
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
    jitter_unit,
    mcp_call,
    mcp_result,
)

# Ретраи handshake: листовой цикл, транзиентное повторяемо (зеркально
# affine_client.rs: HANDSHAKE_TRIES, тот же бэкоф-планировщик).
HANDSHAKE_TRIES = 3

# Общая сессия с uwuwu-cli (Rust): initialize = sign-in афины 4-7с на каждый
# запуск — главный источник латентности python-тулз. Файл разделяемый под
# lock'ом ~/.uwuwu/mcp-session.lock; протухшую сессию call() пере-handshake'ит
# сам (паттерн affine_client.rs).
SESSION_FILE = Path.home() / ".uwuwu" / "mcp-session.txt"
# Lock-файл НИКОГДА не удаляется и не перезаписывается: session-файл
# переписывается handshake'ами, лок на нём ломался бы сменой inode.
LOCK_FILE = Path.home() / ".uwuwu" / "mcp-session.lock"
_live_sid: str | None = None
_lock_fd: int | None = None


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


def _handshake(client_name: str = "wiki-ops", publish: bool = True) -> str:
    """Сессия с ретраями транзиентного (HTTP 429/5xx, URLError/timeout) на
    общем бэкоф-планировщике; McpError от mcp_result (isError/JSON-RPC
    error) — фатально, сразу наверх с причиной. publish=False — приватная
    сессия: общий файл не пишем. ВСЕ пути пере-handshake'а наследуют режим
    старта — иначе private-процесс подменил бы общую сессию своей.
    client_name виден на уровне ПРОТОКОЛА (initialize.clientInfo,
    MCP-трейс/инспектор); stdout шлюза логирует только UUID сессий."""
    for attempt in range(HANDSHAKE_TRIES):
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
            if publish:
                _save_session(sid)
            return sid
        except McpError:
            if sid:
                close(sid)
            raise
        except urllib.error.HTTPError as e:
            if sid:
                close(sid)
            if e.code not in (429, 502, 503):
                raise
            _retry_wait(
                "handshake", attempt, HANDSHAKE_TRIES - 1, str(e)[:60], _retry_after_secs(e)
            )
        except (urllib.error.URLError, TimeoutError, socket.timeout) as e:
            if sid:
                close(sid)
            _retry_wait("handshake", attempt, HANDSHAKE_TRIES - 1, str(e)[:60], None)
        # успешный путь: sid возвращается живым — close только в НЕУСПЕШНЫХ ветках
        # (finally закрыл бы сессию и при return — call после session ловил 404)
    raise RuntimeError("unreachable")


def session(client_name: str = "wiki-ops") -> str:
    """Переиспользовать сохранённую сессию (общая с uwuwu-cli) под локом,
    иначе полный handshake: лок наш → shared (публикуем в общий файл),
    лок чужой → private (своя сессия, файл не трогаем вовсе — он
    принадлежит живому процессу). Протухшая сессия ловится в call().
    client_name — идентичность создателя на уровне протокола initialize
    (MCP-трейс); в stdout-логах шлюза её нет, там только UUID."""
    global _live_sid
    if _live_sid:
        return _live_sid
    if _acquire_session_lock():
        saved = _load_saved_session()
        if saved:
            return saved
        _live_sid = _handshake(client_name, publish=True)
        return _live_sid
    _live_sid = _handshake(client_name, publish=False)
    return _live_sid


def _acquire_session_lock() -> bool:
    """flock на выделенном lock-файле до конца процесса (ядерная семантика:
    крэш освобождает лок сам). True — мы владелец общей сессии."""
    global _lock_fd
    if _lock_fd is not None:
        return True
    try:
        LOCK_FILE.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(LOCK_FILE, os.O_RDWR | os.O_CREAT, 0o644)
    except OSError:
        return False
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)  # лок занят живым процессом — работаем приватно
        return False
    _lock_fd = fd
    return True


def _env_int(name: str, default: int, lo: int, hi: int) -> int:
    """Env-оверрайд с валидацией: мусор → warning + дефолт, значения клампятся."""
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        return max(lo, min(hi, int(raw.strip())))
    except ValueError:
        print(
            f"wiki-ops: {name}={raw!r} не число — беру дефолт {default}",
            file=sys.stderr,
            flush=True,
        )
        return default


def _retry_retries() -> int:
    return _env_int("UWUWU_RETRY_ATTEMPTS", 3, 0, 12)


def _retry_base_ms() -> int:
    return _env_int("UWUWU_RETRY_BASE_MS", 1000, 0, 60000)


_RETRY_AFTER_CAP_S = 30.0  # 503 с «спи 10 минут» не спим дольше полминуты
_BACKOFF_CAP_S = 20.0


def _backoff_pause(attempt: int, retry_after: float | None) -> float:
    """Пауза повтора: Retry-After шлюза приоритетен (кламп 30с — длинный
    outage клиент не переживает, честнее ошибка с подсказкой), иначе
    base·2^attempt с клампом 20с и full jitter. Зеркально affine_client.rs."""
    if retry_after is not None:
        return min(retry_after, _RETRY_AFTER_CAP_S)
    delay = min((_retry_base_ms() / 1000.0) * (2**attempt), _BACKOFF_CAP_S)
    return jitter_unit() * delay


def _retry_after_secs(e: urllib.error.HTTPError) -> float | None:
    raw = e.headers.get("Retry-After") if e.headers else None
    try:
        return float(raw)
    except (TypeError, ValueError):
        return None


def _retry_wait(
    what: str, attempt: int, retries: int, reason: str, retry_after: float | None
) -> None:
    pause = _backoff_pause(attempt, retry_after)
    print(
        f"wiki-ops: {what}: повтор {attempt + 1}/{retries} через {pause:.0f}s: {reason}",
        file=sys.stderr,
        flush=True,
    )
    time.sleep(pause)


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


def _validated(name: str, r: object) -> dict:
    """mcp_result с префиксом имени инструмента (см. докстринг call)."""
    try:
        return mcp_result(r)
    except McpError as e:
        raise McpError(f"{name}: {e}") from e


# Чтения — закрытый список префиксов; всё остальное = write (safe default:
# новый read-тулз теряет ретраи, но запись никогда не дублируется).
_READ_PREFIXES = ("get_", "list_", "search_", "find_", "read_", "export_", "analyze_")

# Коды, при которых шлюз по построению НЕ применял запрос: WRITE_QUEUE_* —
# ждало в очереди и не диспеттилось; rate_limited — отклонено до обработки.
# Остальные true-коды для записи не доверяются (частичный move, trash по
# socket-таймауту и т.п. — операция могла примениться). Неизвестный код =
# не доверяем. Список синхронен write_retryable_trusted() в
# cli/src/affine_client.rs; обоснование — ADR-006 (uwuwu).
_WRITE_TRUSTED_CODES = frozenset({"WRITE_QUEUE_TIMEOUT", "WRITE_QUEUE_FULL", "rate_limited"})


def _is_read(name: str) -> bool:
    return name.startswith(_READ_PREFIXES)


def _ok_false_candidates(result: dict) -> list[object]:
    """Места, где шлюз может нести ok:false-пейлоад: structuredContent,
    result верхнего уровня и распарсенные content[0].text-блобы."""
    candidates: list[object] = [result.get("structuredContent"), result]
    for c in result.get("content") or []:
        if isinstance(c, dict) and isinstance(c.get("text"), str):
            try:
                candidates.append(json.loads(c["text"]))
            except json.JSONDecodeError:
                pass  # не-JSON блоб — не ok:false-пейлоад
    return candidates


def _ok_false(r: object) -> tuple[str, bool, str | None] | None:
    """(detail, retryable, code), если ответ несёт шлюзовую конвенцию
    логического фейла ok:false — в structuredContent, в result верхнего
    уровня или внутри content[0].text-блоба (зеркально affine_client.rs).
    Вызывается ДО mcp_result, который любой ok:false превращает в McpError
    без флага. retryable:true трактуется по типу операции: чтению доверяем
    всегда, записи — только кодам _WRITE_TRUSTED_CODES (ADR-006)."""
    if not isinstance(r, dict):
        return None
    result = r.get("result")
    if not isinstance(result, dict):
        return None
    for cand in _ok_false_candidates(result):
        if isinstance(cand, dict) and cand.get("ok") is False:
            detail = cand.get("error") or "unknown tool error"
            if not isinstance(detail, str):
                detail = json.dumps(detail, ensure_ascii=False)
            code = cand.get("code")
            return (detail, cand.get("retryable") is True, code if isinstance(code, str) else None)
    return None


def _retry_trusted(name: str, retryable: bool, code: str | None) -> bool:
    """Доверие ok:false+retryable:true по типу операции (ADR-006): чтение
    ничего не применяет — флаг доверяем при любом коде; запись — только
    кодам, при которых шлюз гарантированно не применял запрос."""
    return retryable and (_is_read(name) or code in _WRITE_TRUSTED_CODES)


def call(sid: str, name: str, args: dict) -> dict:
    """tools/call с восстановлением протухшей сессии (400/404 → один
    пере-handshake → повтор) и ретраями с бэкофом: HTTP 429/5xx и сетевые
    отказы — всегда; таймаут/обрыв — только чтение (запись могла
    примениться — честная ошибка без ретрая); ok:false+retryable:true —
    ретрай чтению всегда, записи только при доверенном коде из
    _WRITE_TRUSTED_CODES (ADR-006). Обновлённый sid живёт в модуле —
    последующие call(старый_sid,...) продолжают работать."""
    global _live_sid
    effective = _live_sid or sid
    retries = _retry_retries()
    stale_tried = False
    pause_override: float | None = None  # stale-повтор — без бэкоф-паузы
    retry_after: float | None = None  # Retry-After последнего 429/503
    reason = ""
    for attempt in range(retries + 1):
        if attempt:
            if pause_override is not None:
                pause = pause_override
            else:
                pause = _backoff_pause(attempt - 1, retry_after)
            pause_override = None
            retry_after = None
            if pause > 0:
                print(
                    f"wiki-ops: повтор {attempt}/{retries} через {pause:.0f}s: {reason[:60]}",
                    file=sys.stderr,
                    flush=True,
                )
                time.sleep(pause)
        try:
            r, _ = mcp_call(
                "tools/call", {"name": name, "arguments": args}, effective
            )
        except urllib.error.HTTPError as e:
            if e.code in (400, 404) and not stale_tried:
                # сессия протухла: полный handshake в РЕЖИМЕ СТАРТА (shared
                # публикует в общий файл, private — нет) и повтор
                stale_tried = True
                e.close()  # отпустить соединение до повторного запроса
                print(
                    "wiki-ops: сессия протухла — пере-handshake",
                    file=sys.stderr,
                    flush=True,
                )
                effective = _handshake(publish=_lock_fd is not None)
                _live_sid = effective
                pause_override = 0.0
                reason = "сессия протухла"
                continue
            if e.code in (429, 502, 503):
                reason = f"HTTP {e.code}"
                retry_after = _retry_after_secs(e)
                continue
            raise
        except (urllib.error.URLError, TimeoutError, socket.timeout) as e:
            if not _is_read(name):
                raise McpError(
                    f"{name}: таймаут/обрыв ответа шлюза — операция могла "
                    "примениться, проверь результат перед повтором"
                ) from e
            reason = str(e)[:60]
            continue
        failure = _ok_false(r)
        if failure is not None:
            detail, retryable, code = failure
            if _retry_trusted(name, retryable, code) and attempt < retries:
                reason = detail
                continue
            if retryable and not _retry_trusted(name, retryable, code):
                raise McpError(
                    f"{name}: {detail} — retryable:true от шлюза для записи "
                    f"не доверяется (код {code!r} вне whitelist, операция могла "
                    "примениться; см. ADR-006 uwuwu)"
                )
            raise McpError(f"{name}: {detail}")
        return _validated(name, r)
    raise McpError(
        f"{name}: шлюз не ответил за {retries + 1} попыток ({reason}) — "
        "если шлюз лежит целиком, держи паузу 120-180с между попытками"
    )


def _cleanup_private_session() -> None:
    """Private-сессия (старт при чужом локе или mid-run пере-handshake в
    private-режиме) умирает вместе с процессом — вернём слот шлюзу.
    Shared-сессию НЕ закрываем: она опубликована в общем файле."""
    if _live_sid and _lock_fd is None:
        close(_live_sid)


atexit.register(_cleanup_private_session)


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
    try:
        sys.exit(main())
    except McpError as e:
        print(f"!! {e}", file=sys.stderr)
        sys.exit(2)
