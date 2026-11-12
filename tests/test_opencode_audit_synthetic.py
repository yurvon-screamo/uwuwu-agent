"""opencode_audit.py: synthetic-сообщения несут текст в дампе (parts + timeline preview).

Контекст (задача 20261112074307-zq2iv3o): opencode v2 хранит текст synthetic
(«The server restarted while you were working», «The previous response was
interrupted», «[sticky-retry] provider outage») в `data.text`, но загрузчик
поднимал в parts только user-текст и assistant-content — timeline-превью
synthetic было всегда пустым, и дример по дампу не отличал рестарт serve от
instruction-вставки. Фикс: else-ветка `_load_session_message` поднимает
`data.text` для роли synthetic; `_message_preview` отдаёт `_first_text` для
synthetic. Красное состояние на master воспроизводится этим же тестом
(red/green проверено на вилке master/ветка).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
"""
import json
import sqlite3
import unittest

from tools import opencode_audit as audit

SESSION_ID = "ses_test_synthetic"


def _in_memory_db() -> sqlite3.Connection:
    con = sqlite3.connect(":memory:")
    con.row_factory = sqlite3.Row
    con.execute(
        """CREATE TABLE session_message (
            id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
            seq integer NOT NULL, time_created integer NOT NULL,
            time_updated integer NOT NULL, data text NOT NULL)"""
    )
    return con


def _insert(con: sqlite3.Connection, seq: int, msg_type: str, data: dict) -> None:
    con.execute(
        "INSERT INTO session_message VALUES (?, ?, ?, ?, ?, ?, ?)",
        (f"msg_{seq:03d}", SESSION_ID, msg_type, seq, 1000 + seq, 1000 + seq,
         json.dumps(data)),
    )


class SyntheticPreviewTest(unittest.TestCase):
    def setUp(self):
        self.con = _in_memory_db()
        _insert(self.con, 1, "user", {"time": {"created": 1001}, "text": "промпт фабрики"})
        _insert(self.con, 2, "synthetic", {
            "time": {"created": 1002},
            "text": "The server restarted while you were working. Continue from where you left off.",
        })
        _insert(self.con, 3, "synthetic", {
            "time": {"created": 1003},
            "text": "Instructions from: /repo/AGENTS.md",
        })
        # shell-нотификация: text нет, только metadata — preview обязан остаться пустым
        _insert(self.con, 4, "synthetic", {
            "time": {"created": 1004},
            "metadata": {"source": "shell", "state": "completed"},
        })

    def test_synthetic_text_reaches_parts(self):
        content = audit.load_session_content(self.con, SESSION_ID)
        synth = [b for b in content if b["role"] == "synthetic"]
        self.assertEqual(len(synth), 3)
        self.assertEqual(
            audit._first_text(synth[0]["parts"]),
            "The server restarted while you were working. Continue from where you left off.",
        )
        self.assertEqual(audit._first_text(synth[1]["parts"]), "Instructions from: /repo/AGENTS.md")
        self.assertEqual(audit._first_text(synth[2]["parts"]), "")

    def test_synthetic_timeline_preview_carries_text(self):
        content = audit.load_session_content(self.con, SESSION_ID)
        timeline = audit.extract_timeline(content, 10)
        syn_previews = [t["preview"] for t in timeline if t["role"] == "synthetic"]
        self.assertTrue(any("server restarted" in p for p in syn_previews))
        self.assertTrue(any("Instructions from:" in p for p in syn_previews))
        self.assertTrue(any(p == "" for p in syn_previews))

    def test_message_preview_role_branches(self):
        self.assertEqual(audit._message_preview("synthetic", [{"type": "text", "text": "x"}]), "x")
        self.assertEqual(audit._message_preview("synthetic", []), "")
        self.assertEqual(audit._message_preview("idle", [{"type": "text", "text": "x"}]), "")
        self.assertEqual(audit._message_preview("user", [{"type": "text", "text": "u"}]), "u")


if __name__ == "__main__":
    unittest.main()
