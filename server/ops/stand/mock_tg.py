#!/usr/bin/env python3
"""mock_tg.py — эмулятор Telegram Bot API для мок-стенда uwuwu-server (#1150).

Endpoints:
  POST /bot<token>/getUpdates  — long-poll очереди (timeout из тела)
  POST /bot<token>/sendMessage — записать отправку
  POST /inject                 — {update_id, chat_type, chat_id, username, text}
  GET  /sent                   — {"ok":true,"sent":[{chat_id,text},…]}
  POST /reset                  — очистить очередь и отправки

Запуск: MOCK_TG_PORT=8098 python3 ops/stand/mock_tg.py
"""
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Condition()
UPDATES = []  # [{update_id, message:{chat:{id,type}, from:{username}, text}}]
SENT = []


def _body(handler):
    length = int(handler.headers.get("Content-Length") or 0)
    raw = handler.rfile.read(length) if length else b"{}"
    try:
        return json.loads(raw or b"{}")
    except json.JSONDecodeError:
        return {}


def _reply(handler, payload, status=200):
    data = json.dumps(payload).encode()
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(data)))
    handler.end_headers()
    handler.wfile.write(data)


class Api(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # тишина: логи сервера читает e2e
        pass

    def do_GET(self):
        if self.path == "/sent":
            with LOCK:
                out = list(SENT)
            _reply(self, {"ok": True, "sent": out})
        else:
            _reply(self, {"ok": False, "description": "not found"}, 404)

    def do_POST(self):
        body = _body(self)
        if self.path == "/inject":
            msg = {
                "update_id": int(body["update_id"]),
                "message": {
                    "chat": {"id": body.get("chat_id", 42), "type": body.get("chat_type", "private")},
                    "from": {"username": body.get("username", "yurvon")},
                    "text": body.get("text", ""),
                },
            }
            with LOCK:
                UPDATES.append(msg)
                LOCK.notify_all()
            _reply(self, {"ok": True})
        elif self.path == "/reset":
            with LOCK:
                UPDATES.clear()
                SENT.clear()
            _reply(self, {"ok": True})
        elif self.path.endswith("/getUpdates"):
            offset = int(body.get("offset") or 0)
            timeout = float(body.get("timeout") or 0)
            deadline = time.monotonic() + timeout
            with LOCK:
                while True:
                    pending = sorted(
                        (u for u in UPDATES if u["update_id"] >= offset),
                        key=lambda u: u["update_id"],
                    )
                    if pending:
                        _reply(self, {"ok": True, "result": pending})
                        return
                    left = deadline - time.monotonic()
                    if left <= 0:
                        _reply(self, {"ok": True, "result": []})
                        return
                    LOCK.wait(left)
        elif self.path.endswith("/sendMessage"):
            with LOCK:
                SENT.append({"chat_id": body.get("chat_id"), "text": body.get("text", "")})
            _reply(self, {"ok": True, "result": {"message_id": len(SENT)}})
        else:
            _reply(self, {"ok": False, "description": "not found"}, 404)


if __name__ == "__main__":
    port = int(__import__("os").environ.get("MOCK_TG_PORT", "8098"))
    print(f"mock-tg up on :{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Api).serve_forever()
