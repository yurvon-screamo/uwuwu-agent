#!/usr/bin/env python3
"""wiki_ops — CLI-хелпер MCP-операций над доками AFFiNE (для сабагентов).

Сессия: последовательные запуски шарят ~/.uwuwu/mcp-session.txt с uwuwu-cli
(initialize = sign-in афины 4-7с — платим только когда сохранённая протухла,
call() сам пере-handshake'ится: 400/404 → один ретрай). Эксклюзивность —
flock на ~/.uwuwu/mcp-session.lock: лок наш → шарим и публикуем handshake в
общий файл; лок чужой → private-сессия, файл не читаем и не пишем. JSON-RPC
id — случайная база + счётчик (общая сессия с коллизией id давала кросс-ток
ответов, задача IyzxmFdujA). call() ретраит транзиентные сбои шлюза
(429/5xx/сеть, ok:false+retryable) с экспоненциальным бэкофом; таймаут на
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
import http.client
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
    NonJsonResponseError,
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


class _HandshakeRetry(Exception):
    """Транзиентный отказ initialize (HTTP 429/5xx, connect-ошибка) —
    попытка повторяется на бэкоф-планировщике."""


def _handshake_verdict(e: Exception) -> None:
    """Вердикт по отказу initialize (зеркально initialize_once в
    affine_client.rs): ретраится только транзиентное (HTTP 429/5xx,
    connect-ошибки); таймаут НЕ ретраится — осиротевшая сессия жрёт слот
    шлюза до idle-timeout; остальное уходит как есть. Всегда рейзит."""
    if isinstance(e, (TimeoutError, socket.timeout)):
        raise e
    if isinstance(e, urllib.error.HTTPError):
        if e.code in (429, 502, 503):
            raise _HandshakeRetry(str(e)[:60]) from e
        raise e
    if isinstance(e, urllib.error.URLError):
        if not _is_timeout_reason(e):
            raise _HandshakeRetry(str(e)[:60]) from e
        raise e
    raise e


def _handshake_once(client_name: str) -> str:
    """Одна попытка initialize+initialized; успех → живой sid (НЕ закрыт),
    недонародившаяся сессия закрывается; вердикт по отказу — у
    _handshake_verdict (он всегда рейзит)."""
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
        return sid
    except Exception as e:  # noqa: BLE001 — вердикт ниже рейзит всегда
        if sid:
            close(sid)
        _handshake_verdict(e)
        raise  # недостижимо: verdict всегда рейзит; страховка типизатору


def _is_timeout_reason(e: urllib.error.URLError) -> bool:
    return isinstance(getattr(e, "reason", None), (socket.timeout, TimeoutError))


def _handshake(client_name: str = "wiki-ops", publish: bool = True) -> str:
    """Сессия с ретраями транзиентного (HTTP 429/5xx, connect-ошибки) на
    общем бэкоф-планировщике; McpError/таймаут — фатально. publish=False —
    приватная сессия: общий файл не пишем. ВСЕ пути пере-handshake'а
    наследуют режим старта — иначе private-процесс подменил бы общую
    сессию своей. client_name виден на уровне ПРОТОКОЛА
    (initialize.clientInfo, MCP-трейс/инспектор); stdout шлюза логирует
    только UUID сессий."""
    last: Exception | None = None
    for attempt in range(HANDSHAKE_TRIES):
        if attempt:
            _retry_wait("handshake", attempt, HANDSHAKE_TRIES - 1, str(last)[:60], None)
        try:
            sid = _handshake_once(client_name)
        except _HandshakeRetry as e:
            last = e
            continue
        if publish:
            _save_session(sid)
        return sid
    raise McpError(
        f"handshake не удался за {HANDSHAKE_TRIES} попыток: {last}"
    ) from last


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

# Обрыв/потеря ответа (класс Ambiguous в affine_client.rs): ответ мог не
# дойти — чтение ретраится, запись получает честное «могла примениться».
_AMBIGUOUS_EXC = (
    TimeoutError,
    socket.timeout,
    ConnectionError,
    http.client.IncompleteRead,
    NonJsonResponseError,
)

# Классифицируемые отказы попытки (остальные летят наверх как есть).
_CALL_CATCH = (
    urllib.error.HTTPError,
    urllib.error.URLError,
    TimeoutError,
    socket.timeout,
    NonJsonResponseError,
    ConnectionError,
    http.client.IncompleteRead,
)


def _is_read(name: str) -> bool:
    return name.startswith(_READ_PREFIXES)


def _ambiguous_exc(e: Exception) -> bool:
    if isinstance(e, urllib.error.URLError):
        return _is_timeout_reason(e)
    return isinstance(e, _AMBIGUOUS_EXC)


def _call_failure_action(
    e: Exception, name: str, stale_tried: bool
) -> tuple[str, Exception | float | None, str]:
    """Классификация отказа попытки — зеркально GatewayError в
    affine_client.rs. Возвращает (action, payload, reason):
    "stale" — один пере-handshake и повтор; "retry" — повтор на бэкофе
    (payload = Retry-After или None); "fatal" — payload = исключение для
    ре-рейза (запись при Ambiguous получает честное «могла примениться»)."""
    if isinstance(e, urllib.error.HTTPError):
        if e.code in (400, 404):
            if stale_tried:
                return (
                    "fatal",
                    McpError(f"{name}: сессия снова отвергнута ({e.code})"),
                    str(e),
                )
            e.close()  # отпустить соединение до повторного запроса
            return ("stale", None, "сессия протухла")
        if e.code in (429, 502, 503):
            return ("retry", _retry_after_secs(e), f"HTTP {e.code}")
        return ("fatal", e, str(e))
    if isinstance(e, urllib.error.URLError) and not _is_timeout_reason(e):
        # connect-отказ (рестарт контейнера, dns): шлюз заведомо не отвечал —
        # ретраим как транспорт для обоих типов, зеркально GatewayError::Transport
        return ("retry", None, str(e)[:60])
    if _ambiguous_exc(e):
        if not _is_read(name):
            return (
                "fatal",
                McpError(
                    f"{name}: таймаут/обрыв ответа шлюза — операция могла "
                    "примениться, проверь результат перед повтором"
                ),
                str(e)[:60],
            )
        return ("retry", None, str(e)[:60])
    return ("fatal", e, str(e)[:60])


def _attempt_call(
    effective: str, name: str, args: dict, stale_tried: bool, retries: int, attempt: int
) -> tuple[str, object, str]:
    """Одна попытка tools/call с полной классификацией исхода
    (зеркально итерации retry-цикла call_tool в affine_client.rs):
    ("ok", result, "") — валидный ответ (ok:false не было);
    ("retry", retry_after|None, reason) — повторить;
    ("stale", None, reason) — пере-handshake выполнен, повторить;
    ("fatal", exc, reason) — рейзить exc (ok:false, 4xx, запись при
    таймауте с честным «могла примениться»)."""
    try:
        r, _ = mcp_call("tools/call", {"name": name, "arguments": args}, effective)
    except _CALL_CATCH as e:
        action, payload, why = _call_failure_action(e, name, stale_tried)
        if action == "stale":
            global _live_sid
            print(
                "wiki-ops: сессия протухла — пере-handshake",
                file=sys.stderr,
                flush=True,
            )
            _live_sid = _handshake(publish=_lock_fd is not None)
            return ("stale", None, why)
        if action == "fatal":
            return ("fatal", payload, why)
        return ("retry", payload if isinstance(payload, float) else None, why)
    failure = _ok_false(r)
    if failure is None:
        return ("ok", r, "")
    detail, retryable = failure
    if retryable and attempt < retries:
        return ("retry", None, detail)
    return ("fatal", McpError(f"{name}: {detail}"), detail)


def _ok_false_candidates(result: dict) -> list:
    """Кандидаты на шлюзовую конвенцию ok:false — порядок зеркален
    tool_failure в affine_client.rs: structuredContent, result верхнего
    уровня, распарсенные content[0].text-блобы."""
    candidates = [result.get("structuredContent"), result]
    for c in result.get("content") or []:
        if isinstance(c, dict) and isinstance(c.get("text"), str):
            try:
                inner = json.loads(c["text"])
            except json.JSONDecodeError:
                continue
            candidates.append(inner)
    return candidates


def _ok_false(r: object) -> tuple[str, bool] | None:
    """(detail, retryable), если ответ несёт шлюзовую конвенцию логического
    фейла ok:false. Вызывается ДО mcp_result, который любой ok:false
    превращает в McpError без флага. retryable:true = шлюз гарантирует,
    что запрос НЕ применён (контракт, задача W9etJF1FVm) — такой сбой
    ретраится и на записи."""
    if not isinstance(r, dict):
        return None
    result = r.get("result")
    if not isinstance(result, dict):
        return None
    for cand in _ok_false_candidates(result):
        if isinstance(cand, dict) and cand.get("ok") is False:
            return _fail_detail(cand)
    return None


def _fail_detail(cand: dict) -> tuple[str, bool]:
    detail = cand.get("error") or "unknown tool error"
    if not isinstance(detail, str):
        detail = json.dumps(detail, ensure_ascii=False)
    return (detail, cand.get("retryable") is True)


def call(sid: str, name: str, args: dict) -> dict:
    """tools/call с восстановлением протухшей сессии (400/404 → один
    пере-handshake → повтор) и ретраями с бэкофом: HTTP 429/5xx и сетевые
    отказы — всегда; таймаут/обрыв — только чтение (запись могла
    примениться — честная ошибка без ретрая); ok:false+retryable:true —
    ретрай. Обновлённый sid живёт в модуле — последующие
    call(старый_sid,...) продолжают работать.
    Осознанная расходимость с Rust-зеркалом: повторная протухшая сессия
    ПОСЛЕ пере-handshake'а здесь — fail fast («снова отвергнута»), в
    affine_client.rs — дожимаются оставшиеся попытки. Не уравнивать
    вслепую: python-цепочки длиннее, бессмысленные добивания дороже."""
    global _live_sid
    effective = _live_sid or sid
    retries = _retry_retries()
    stale_tried = False
    retry_after: float | None = None
    reason = ""
    for attempt in range(retries + 1):
        if attempt:
            pause = _backoff_pause(attempt - 1, retry_after)
            retry_after = None
            print(
                f"wiki-ops: повтор {attempt}/{retries} через {pause:.0f}s: {reason[:60]}",
                file=sys.stderr,
                flush=True,
            )
            time.sleep(pause)
        status, value, why = _attempt_call(
            effective, name, args, stale_tried, retries, attempt
        )
        if status == "ok":
            return _validated(name, value)
        if status == "fatal":
            raise value from None
        if status == "stale":
            stale_tried = True
            effective = _live_sid
        elif isinstance(value, float):
            retry_after = value
        reason = why
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
