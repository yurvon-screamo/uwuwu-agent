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
  create <тайтл> <файл.md> [--parent <docId>]   — создать док (опц. под родителя-ДОК, печатает docId;
                                                --parent валидируется ДО создания: organize-папка или
                                                несуществующий док → отказ без создания сироты)
  link <docId> --folder <folderId>              — прилинковать док в organize-папку (для свежесозданного
                                                дока; идемпотентен)
  org <docId> --to <folderId>                   — переместить СУЩЕСТВУЮЩИЙ organize-линк в другую папку
  move <docId> --parent <docId>                  — перенести под родителя (doc-в-док)
  append <docId> <файл.md>                       — дописать markdown в конец дока
  replace <docId> <файл.md> [--stub]             — заменить контент (--stub: без длиновой защиты)
  export <docId>                                 — живой markdown дока в stdout
  embed <docId> [--html файл] [--name id]        — вставить/заменить embed_html-диаграмму (replace по маркеру ⁂ diagram:<id>)
  orgls                                         — organize-папки (id + имя)
  children <folderId>                           — organize-дети папки (folder/doc-линки, порядок index)
  find <…>                                      — ВЫПИЛЕН: шлюз не отдаёт doc-дерево (list_workspace_tree
                                                возвращает organize-ноды без docId/title); docId по
                                                тайтлу — uwuwu-cli search
  unlink <docId>                                 — снять organize-линк(и) дока из папок (дедуп: убирает из сайдбара; из поиска док уходит только в корзину)
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
        # записи ждут дольше: organize/trash-операции шлюза ходят 240s+,
        # клиентский обрыв на дефолтных 120с при дожившем до успеха сервере
        # превращался в ложный Ambiguous (операция-то применена)
        r, _ = mcp_call(
            "tools/call",
            {"name": name, "arguments": args},
            effective,
            timeout=120 if _is_read(name) else 300,
        )
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


def _content_text(res: object) -> str | None:
    """Первый content[].text из MCP-ответа (обёртки шлюза кладут JSON-полезную
    нагрузку именно там)."""
    if not isinstance(res, dict):
        return None
    for c in res.get("content", []):
        if isinstance(c, dict) and "text" in c:
            return c["text"]
    return None


def _doc_exists(res: object) -> bool | None:
    """Существует ли док по ответу read_doc. Шлюз кладёт verdict в
    structuredContent.exists (дублирует в content[].text-JSON) НА УРОВНЕ
    result, не на верхнем уровне — несуществующий docId приходит УСПЕХОМ
    с exists:false (isError нет). None = вердикта в ответе нет."""
    if not isinstance(res, dict):
        return None
    doc = res.get("structuredContent")
    if not isinstance(doc, dict):
        try:
            doc = json.loads(_content_text(res) or "{}")
        except json.JSONDecodeError:
            return None
    exists = doc.get("exists") if isinstance(doc, dict) else None
    return exists if isinstance(exists, bool) else None


def _validate_parent(sid: str, parent: str) -> int | None:
    """Валидация --parent ДО создания дока: вернуть exit-код отказа (0/None =
    ок) либо напечатать причину. Организе-папка по node.id → подсказка link;
    несуществующий docId шлюз помечает exists:false в УСПЕШНОМ ответе
    read_doc (isError нет); отказ шлюза (McpError — в т.ч. флейк
    «space:load-doc timeout») и сетевые транзиенты не считаются
    «не существует» — честный rc=2, сироты нет в любом исходе."""
    nodes = _org_nodes(sid)
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
    except McpError as e:  # noqa: S5754 — отказ классифицируем и печатаем подсказку (Ambiguous-текст call уже честный)
        print(
            "read_doc --parent упал с отказом шлюза: parent-док не существует "
            f"ЛИБО шлюз флейкнул — проверь и повтори (отказ: {str(e)[:200]})",
            file=sys.stderr,
        )
        return 2
    exists = _doc_exists(res)
    if exists is None:
        print(
            "read_doc не дал exists-вердикт (схема шлюза изменилась?) — "
            "валидация --parent недоступна, откажись от --parent или повтори",
            file=sys.stderr,
        )
        return 2
    if exists is False:
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
            except McpError as e:  # noqa: S5754 — частичный успех: docId напечатан, печатаем рецепт восстановления; Ambiguous-текст call уже честный («могла примениться»)
                print(
                    f"doc {did} СОЗДАН, но move под {a.parent} не прошёл "
                    f"({str(e)[:200]}); проверь и повтори: move {did} "
                    f"--parent {a.parent}",
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


def cmd_orgls(_a) -> int:
    """Список organize-папок (id + имя) — для поиска folderId."""
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
    """Перенести doc-линк в organize-папку: org <docId> --to <folderId>.
    Перемещает ТОЛЬКО существующий линк — у свежесозданного дока линка нет:
    сначала link <docId> --folder <folderId>."""
    sid = session()
    try:
        nodes = _org_nodes(sid)
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
    """Организе-дети папки: children <folderId> (подпапки + doc-линки,
    порядок index, маркер «# mode: organize»). Doc-дети (док-в-док)
    НЕ поддерживаются: list_workspace_tree шлюза отдаёт organize-ноды
    без docId/title — doc-дерево в этом транспорте недоступно."""
    sid = session()
    try:
        nodes = _org_nodes(sid)
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


def cmd_link(a) -> int:
    """Прилинковать док в organize-папку: link <docId> --folder <folderId>
    (обёртка gateway add_organize_link — единственный способ организовать
    свежесозданный док, у которого линка в organize ещё нет).
    Идемпотентен (инвариант шлюза: один линк на док): линк уже в этой
    папке — «уже здесь», exit 0; в другой — предупреждение «нужен org»,
    exit 1."""
    sid = session()
    try:
        nodes = _org_nodes(sid)
        if not any(n.get("type") == "folder" and n.get("id") == a.folder for n in nodes):
            print(
                f"папка {a.folder} не найдена в organize (см. orgls)",
                file=sys.stderr,
            )
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
            return 2
        exists = _doc_exists(target)
        if exists is None:
            print(
                "read_doc не дал exists-вердикт (схема шлюза изменилась?) — "
                "линк не создаём: проверь docId или повтори",
                file=sys.stderr,
            )
            return 2
        if exists is False:
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
        try:
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
        except McpError as e:  # noqa: S5754 — мутация оборвана отказом шлюза: могла дожить, печатаем рецепт проверки
            print(
                f"add_organize_link не прошёл ({str(e)[:180]}); отказ/обрыв ≠ "
                f"ПРОВАЛ — проверь orgls перед повтором link {a.docId} "
                f"--folder {a.folder}",
                file=sys.stderr,
            )
            return 2
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
    lk = sub.add_parser("link")
    lk.add_argument("docId")
    lk.add_argument("--folder", required=True)
    u = sub.add_parser("unlink")
    u.add_argument("docId")
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
            "unlink": cmd_unlink,
        }[a.cmd](a)
    except McpError as e:
        # контракт docstring McpError: exit-код — забота CLI-точки входа;
        # 2 = отказ шлюза (в отличие от 1 — наших валидационных отказов до мутаций)
        print(f"!! {e}", file=sys.stderr)
        return 2
    except (urllib.error.URLError, ConnectionError, TimeoutError) as e:
        # URLError — сбои соединения (HTTP-коды в т.ч. детерминированные),
        # ConnectionError/TimeoutError — обрыв/таймаут мимо классификации call
        # (ловим сырыми как страховку). rc=2 = «шлюз/сеть», единая точка
        # rc-семантики вместо traceback'а в CLI-обвязках
        print(f"!! шлюз/сеть: {str(e)[:200]}", file=sys.stderr)
        print(
            "!! если это был таймаут — мутация могла дожить до успеха: "
            "проверь результат перед повтором",
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    # rc-семантика переведена внутрь main() — единая точка
    sys.exit(main())
