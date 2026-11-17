#!/usr/bin/env python3
"""mock_tg.py — эмулятор Telegram Bot API для мок-стенда uwuwu-server (#1150).

Endpoints:
  POST /bot<token>/getUpdates  — long-poll очереди (timeout из тела)
  POST /bot<token>/sendMessage — записать отправку
  POST /bot<token>/getFile     — file_path по file_id; >20 MiB — 400 too big (#1171)
  GET  /file/bot<token>/<path> — байты файла (сохранены при inject)
  POST /inject                 — {update_id, chat_type, chat_id, username, text[, media]}
  GET  /sent                   — {"ok":true,"sent":[{chat_id,text},…]}
  POST /reset                  — очистить очередь и отправки

media в inject (e2e аттачей, #1171):
  {"kind":"photo"|"document"|"video", "file_id":"x",
   "file_name":"скрин.png", "file_size":12345, "caption":"…"}

Запуск: MOCK_TG_PORT=8098 python3 ops/stand/mock_tg.py
"""
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Condition()
UPDATES = []  # [{update_id, message:{chat:{id,type}, from:{username}, text}}]
SENT = []
FILES = {}  # file_id -> bytes
SIZES = {}  # file_id -> заявленный размер (oversize-эмуляция getFile)
FILE_LIMIT = 20 * 1024 * 1024  # 20 MiB — потолок скачивания Bot API


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
    handler.send_header("Content-Length", str(data and len(data) or 0))
    handler.end_headers()
    handler.wfile.write(data)


def _media_message(body):
    """Апдейт с media-вложением (#1171): photo/document/other-ключи."""
    kind = body.get("kind", "photo")
    file_id = body.get("file_id", "f1")
    size = body.get("file_size")
    raw = body.get("bytes", "mock-file-bytes")
    FILES[file_id] = raw.encode() if isinstance(raw, str) else raw
    SIZES[file_id] = size if size is not None else len(FILES[file_id])
    media = {"file_id": file_id}
    if size is not None:
        media["file_size"] = size
    if kind == "document":
        media["file_name"] = body.get("file_name", "file.bin")
        media["mime_type"] = "application/octet-stream"
        return {"document": media}
    if kind == "photo":
        return {"photo": [{"file_id": file_id, "file_size": size or 100}]}
    return {kind: media}


class Api(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # тишина: логи сервера читает e2e
        pass

    def do_GET(self):
        if self.path == "/sent":
            with LOCK:
                out = list(SENT)
            _reply(self, {"ok": True, "sent": out})
        elif self.path.startswith("/file/bot"):
            # GET /file/bot<token>/<path> — путь заканчивается file_id
            file_id = self.path.rsplit("/", 1)[-1]
            data = FILES.get(file_id)
            if data is None:
                _reply(self, {"ok": False, "description": "file not found"}, 404)
                return
            handler = self
            handler.send_response(200)
            handler.send_header("Content-Type", "application/octet-stream")
            handler.send_header("Content-Length", str(len(data)))
            handler.end_headers()
            handler.wfile.write(data)
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
            if body.get("media"):
                media = _media_message(body["media"])
                msg["message"].pop("text", None)
                msg["message"].update(media)
                if body["media"].get("caption"):
                    msg["message"]["caption"] = body["media"]["caption"]
            with LOCK:
                UPDATES.append(msg)
                LOCK.notify_all()
            _reply(self, {"ok": True})
        elif self.path == "/reset":
            with LOCK:
                UPDATES.clear()
                SENT.clear()
                FILES.clear()
                SIZES.clear()
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
        elif self.path.endswith("/getFile"):
            file_id = body.get("file_id", "")
            size = SIZES.get(file_id, 0)
            # перманентный отказ API на файлы > 20 MiB (#1171)
            if size > FILE_LIMIT:
                _reply(
                    self,
                    {"ok": False, "error_code": 400, "description": "Bad Request: file is too big"},
                    400,
                )
                return
            _reply(self, {"ok": True, "result": {"file_id": file_id, "file_path": f"bot-files/{file_id}"}})
        else:
            _reply(self, {"ok": False, "description": "not found"}, 404)


if __name__ == "__main__":
    port = int(__import__("os").environ.get("MOCK_TG_PORT", "8098"))
    print(f"mock-tg up on :{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Api).serve_forever()
