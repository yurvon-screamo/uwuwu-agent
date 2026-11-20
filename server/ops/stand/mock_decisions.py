#!/usr/bin/env python3
"""mock_decisions.py — эмулятор Google Decisions API для e2e-фазы судьи (#1364).

  POST /decisions — canned answers: choice из последнего /set (дефолт hold-подобный)
  POST /set       — {"status": "In Progress"} — вердикт следующих вызовов
  GET  /calls     — {"calls": N} — сколько вызовов судьи принял
  POST /reset     — обнулить счётчик

Запуск: MOCK_JUDGE_PORT=8097 python3 ops/stand/mock_decisions.py
"""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
CALLS = 0
STATUS = "In Progress"  # дефолт вердикта фазы «шум гасится»
NO_ROUTE = {"error": "no route"}

CHOICE_LABELS = {
    "InProgress": "In Progress",
    "Design": "Design",
    "ReviewPlan": "Ревью плана",
    "ReviewResult": "Ревью результата",
    "Testing": "Тестирование",
    "NeedsAttention": "Needs attention",
    "Hold": "hold",
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # шум мока не нужен
        pass

    def _json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        global CALLS, STATUS
        if self.path != "/decisions":
            self._json(404, NO_ROUTE)
            return
        length = int(self.headers.get("Content-Length", "0"))
        self.rfile.read(length)
        with LOCK:
            CALLS += 1
            status = STATUS
        self._json(200, {
            "answers": {
                "next_status": {
                    "type": "choice",
                    "choice": CHOICE_LABELS.get(status, "hold"),
                    "probabilities": {CHOICE_LABELS.get(status, "hold"): 0.8},
                    "confidence": 0.8,
                },
                "owner_needed": {
                    "type": "noul",
                    "noul": 0.1 if status != "NeedsAttention" else 0.9,
                },
            },
            "usage": {"total_input_tokens": 500, "total_output_tokens": 0},
        })

    def do_GET(self):
        if self.path == "/calls":
            with LOCK:
                self._json(200, {"calls": CALLS})
            return
        self._json(404, NO_ROUTE)

    def _read(self):
        length = int(self.headers.get("Content-Length", "0"))
        return json.loads(self.rfile.read(length) or b"{}")

    def do_PUT(self):
        global STATUS, CALLS
        if self.path == "/set":
            STATUS = self._read().get("status", "InProgress")
            self._json(200, {"ok": True, "status": STATUS})
            return
        if self.path == "/reset":
            with LOCK:
                CALLS = 0
            self._json(200, {"ok": True})
            return
        self._json(404, NO_ROUTE)


if __name__ == "__main__":
    import os

    port = int(os.environ.get("MOCK_JUDGE_PORT", "8097"))
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
