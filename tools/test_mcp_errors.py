#!/usr/bin/env python3
"""Контракт ошибок MCP-обвязки: McpError(RuntimeError) вместо SystemExit.

Регрессия инцидента 31.10 (ses_e6caab208ffe, крон «Разбор очереди
заявок»): mcp_result при ошибке шлюза делал sys.exit(2) → SystemExit —
BaseException проскальзывал мимо `except Exception` обвязок кронов,
ретрай-драйвер мутаций умирал молча посреди серии.

На старом коде тест красный, на новом — зелёный. Без pytest-инфры
(в репо только vitest-TS): plain-asserts + фейковый urlopen + e2e с
локальным HTTP-сервером, изображающим шлюз.

Запуск: python3 tools/test_mcp_errors.py  (exit 0 = все зелёные)
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import threading
import urllib.error
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import triage_affine as ta  # noqa: E402
import wiki_ops  # noqa: E402

# режим фейкового шлюза: "rpc_error" — JSON-RPC error в HTTP 200,
# "empty" — HTTP 200 с пустым телом (подпись инцидента 31.10)
FAKE_MODE = "rpc_error"

RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, fn) -> None:
    try:
        fn()
        RESULTS.append((name, True, ""))
        print(f"ok   {name}")
    except (Exception, SystemExit) as e:  # SystemExit старого контракта — тоже провал
        RESULTS.append((name, False, repr(e)))
        print(f"FAIL {name}: {e!r}")


class FakeGateway(BaseHTTPRequestHandler):
    """MCP-шлюз-заглушка: любой POST → ошибка контракта с HTTP 200."""

    def do_POST(self) -> None:
        if FAKE_MODE == "empty":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            return
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32000, "message": "fake gateway error"},
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        """Тишина: HTTP-лог заглушки не должен замусоривать вывод тестов."""


def t_mcp_result_empty_payload_raises_mcperror() -> None:
    """Пустой ответ {} (HTTP 200 без тела) → McpError, а не SystemExit."""
    # act + assert
    try:
        ta.mcp_result({})
    except ta.McpError as e:
        assert isinstance(e, RuntimeError), "не наследник RuntimeError"
        assert "шлюз вернул ошибку" in str(e), str(e)
    except SystemExit as e:
        raise AssertionError("SystemExit вместо McpError (старый контракт)") from e
    else:
        raise AssertionError("молча прошёл пустой ответ")


def t_except_exception_catches_gateway_error() -> None:
    """Регрессия инцидента: обвязка с except Exception обязана поймать."""
    # act
    caught = False
    try:
        ta.mcp_result({})
    except Exception:
        caught = True
    except SystemExit:
        pass  # старый контракт: BaseException мимо except Exception
    # assert
    assert caught, "except Exception НЕ поймал ошибку шлюза"


def t_tool_refusal_carries_detail() -> None:
    """isError-отказ инструмента → McpError с текстом отказа."""
    # arrange
    resp = {
        "result": {
            "isError": True,
            "content": [{"type": "text", "text": "doc not found"}],
        }
    }
    # act + assert
    try:
        ta.mcp_result(resp)
    except ta.McpError as e:
        assert "инструмент отказал" in str(e), str(e)
        assert "doc not found" in str(e), str(e)
    else:
        raise AssertionError("isError прошёл молча")


def t_ok_false_refusal_raises() -> None:
    """ok:false (конвенция {ok:false}-в-200) → McpError."""
    try:
        ta.mcp_result({"result": {"ok": False}})
    except ta.McpError as e:
        assert "инструмент отказал" in str(e), str(e)
    else:
        raise AssertionError("ok:false прошёл молча")


def t_jsonrpc_error_raises_with_payload() -> None:
    """JSON-RPC error → McpError с payload ошибки."""
    try:
        ta.mcp_result({"error": {"code": -32000, "message": "boom"}})
    except ta.McpError as e:
        assert "шлюз вернул ошибку" in str(e) and "boom" in str(e), str(e)
    else:
        raise AssertionError("jsonrpc error прошёл молча")


def t_successful_result_passes_through() -> None:
    """Валидный result проходит без исключений."""
    # act + assert
    assert ta.mcp_result({"result": {"ok": True, "x": 1}}) == {"ok": True, "x": 1}


def t_mcp_call_non_json_body_raises() -> None:
    """Не-JSON ответ шлюза → McpError, а не SystemExit."""
    # arrange: фейковый urlopen вместо сети
    class FakeResp:
        status = 200
        headers = {"mcp-session-id": "fake-sid"}

        def read(self) -> bytes:
            return b"<html>502 Bad Gateway</html>"

        def __enter__(self):
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

    orig = ta.urllib.request.urlopen
    ta.urllib.request.urlopen = lambda req, timeout=120: FakeResp()
    try:
        ta.mcp_call("initialize", {})
    except ta.McpError as e:
        assert "не-JSON ответ" in str(e), str(e)
    except SystemExit as e:
        raise AssertionError("SystemExit вместо McpError (старый контракт)") from e
    else:
        raise AssertionError("не-JSON прошёл молча")
    finally:
        ta.urllib.request.urlopen = orig


def t_call_prefixes_tool_name_on_both_paths() -> None:
    """wiki_ops.call ре-рейзит McpError с именем инструмента — обе ветки:
    первичный вызов (400 → пере-handshake) и повтор после него."""
    # arrange: сценарий tools/call(400) → initialize(ок) → notifications →
    # tools/call(ошибка контракта); состояние wiki_ops изолируем от машины
    class StaleSessionError(urllib.error.HTTPError):
        def __init__(self) -> None:
            super().__init__("http://fake/mcp", 400, "stale", {}, None)

    sequence: list[str] = []

    def fake_mcp_call(
        method: str, params: dict, sid=None, notify=False, timeout=None
    ):
        sequence.append(method)
        if len(sequence) == 1:  # первичный tools/call — сессия протухла
            raise StaleSessionError()
        if method == "initialize":  # пере-handshake — успех
            return {"result": {"ok": True}}, "fresh-sid"
        if notify:  # notifications/initialized
            return {}, "fresh-sid"
        return {"error": {"code": -32000, "message": "boom"}}, "fresh-sid"

    orig_call = wiki_ops.mcp_call
    orig_session_file = wiki_ops.SESSION_FILE
    orig_live_sid = wiki_ops._live_sid
    wiki_ops.mcp_call = fake_mcp_call
    wiki_ops.SESSION_FILE = Path("/tmp/opencode/test-mcp-errors/fake-session.txt")
    wiki_ops._live_sid = None
    try:
        wiki_ops.call("sid", "append_markdown", {})
    except wiki_ops.McpError as e:
        # assert: префикс инструмента на повторе после пере-handshake
        assert str(e).startswith("append_markdown:"), str(e)
        assert "шлюз вернул ошибку" in str(e), str(e)
        assert len(sequence) == 4, f"обе ветки не пройдены: {sequence}"
    except SystemExit as e:
        raise AssertionError("SystemExit вместо McpError (старый контракт)") from e
    else:
        raise AssertionError("ошибка шлюза прошла молча")
    finally:
        wiki_ops.mcp_call = orig_call
        wiki_ops.SESSION_FILE = orig_session_file
        wiki_ops._live_sid = orig_live_sid


def run_cli(
    script: str, argv: list[str], port: int, extra_env: dict
) -> subprocess.CompletedProcess:
    env = {**os.environ, "UWUWU_AFFINE_URL": f"http://127.0.0.1:{port}/mcp", **extra_env}
    return subprocess.run(
        [sys.executable, str(TOOLS / script), *argv],
        capture_output=True,
        text=True,
        timeout=120,
        env=env,
    )


def assert_gateway_error_to_stderr(proc: subprocess.CompletedProcess) -> None:
    """Контракт CLI-точки: rc=2, `!!`-строка в STDERR (не в stdout)."""
    assert proc.returncode == 2, f"rc={proc.returncode}; stderr={proc.stderr[-300:]}"
    assert "шлюз вернул ошибку" in proc.stderr, f"stderr={proc.stderr[-300:]}"
    assert "шлюз вернул ошибку" not in proc.stdout, "сообщение ушло в stdout (старый контракт)"


def t_e2e_wiki_ops_rpc_error_exits_2_with_stderr(port: int) -> None:
    """E2E: jsonrpc error в 200 → wiki_ops CLI: rc=2 + сообщение в stderr."""
    # act
    proc = run_cli("wiki_ops.py", ["orgls"], port, {})
    # assert
    assert_gateway_error_to_stderr(proc)


def t_e2e_wiki_ops_empty_body_exits_2_with_stderr(port: int) -> None:
    """E2E: пустое тело в 200 (подпись инцидента) → rc=2 + stderr."""
    # act
    proc = run_cli("wiki_ops.py", ["orgls"], port, {})
    # assert
    assert_gateway_error_to_stderr(proc)
    assert "{}" in proc.stderr, f"payload пустого ответа потерян; stderr={proc.stderr[-300:]}"


def t_e2e_triage_apply_rpc_error_exits_2_with_stderr(port: int) -> None:
    """E2E: triage_affine apply (dry-run) при ошибке шлюза → rc=2 + stderr."""
    # arrange: чистый фейковый контент-кэш (без протухшего состояния прошлых прогонов)
    fake_wiki = Path("/tmp/opencode/test-mcp-errors/wiki")
    shutil.rmtree(fake_wiki, ignore_errors=True)
    fake_wiki.mkdir(parents=True, exist_ok=True)
    (fake_wiki / "affine_content_cache.json").write_text(
        json.dumps({"2099-01-01": {"doc_id": "fakeDoc123", "markdown": "## s1\nтекст"}}),
        encoding="utf-8",
    )
    # act
    proc = run_cli(
        "triage_affine.py",
        ["apply", "2099-01-01"],
        port,
        {"UWUWU_WIKI": str(fake_wiki), "UWUWU_AFFINE_WORKSPACE_ID": "fake-ws"},
    )
    # assert
    assert_gateway_error_to_stderr(proc)


TESTS = [
    ("mcp_result пустой ответ → McpError", t_mcp_result_empty_payload_raises_mcperror),
    ("except Exception ловит ошибку шлюза", t_except_exception_catches_gateway_error),
    ("isError-отказ несёт детали", t_tool_refusal_carries_detail),
    ("ok:false → McpError", t_ok_false_refusal_raises),
    ("jsonrpc error → McpError с payload", t_jsonrpc_error_raises_with_payload),
    ("валидный result проходит", t_successful_result_passes_through),
    ("mcp_call не-JSON → McpError", t_mcp_call_non_json_body_raises),
    ("call() префиксует имя инструмента", t_call_prefixes_tool_name_on_both_paths),
]


def main() -> int:
    server = HTTPServer(("127.0.0.1", 0), FakeGateway)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for name, fn in TESTS:
            check(name, fn)
        e2e = [
            (
                "rpc_error",
                "e2e wiki_ops: jsonrpc error → rc2/stderr",
                t_e2e_wiki_ops_rpc_error_exits_2_with_stderr,
            ),
            (
                "empty",
                "e2e wiki_ops: пустое тело → rc2/stderr",
                t_e2e_wiki_ops_empty_body_exits_2_with_stderr,
            ),
            (
                "rpc_error",
                "e2e triage apply: ошибка шлюза → rc2/stderr",
                t_e2e_triage_apply_rpc_error_exits_2_with_stderr,
            ),
        ]
        for mode, name, fn in e2e:
            global FAKE_MODE
            FAKE_MODE = mode
            check(name, lambda fn=fn, port=port: fn(port))
    finally:
        server.shutdown()
    failed = [name for name, ok, _ in RESULTS if not ok]
    print(f"\nитого: {len(RESULTS) - len(failed)}/{len(RESULTS)} ok")
    if failed:
        print("провалены: " + "; ".join(failed))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
