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
import subprocess  # nosec B404 — e2e дамп-слоя: запуск собственного CLI в тесте
import sys
import tempfile
import unittest
from pathlib import Path

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


class SyntheticPreviewE2eTest(unittest.TestCase):
    """E2E дамп-слоя: subprocess с файловой БД, полный main-путь до JSON stdout.

    Верхний автоматизируемый уровень (без LLM): фикс-схема v2 (session_v2 +
    session_message) на диске, CLI как его вызовет дример (`session <id> --db
    <путь>`), synthetic-preview обязан доехать до сериализованного timeline.
    """

    SESSION_V2_DDL = """CREATE TABLE session_v2 (
        id text PRIMARY KEY, slug text, title text, agent text,
        directory text, parent_id text,
        summary_additions integer DEFAULT 0, summary_deletions integer DEFAULT 0,
        summary_files integer DEFAULT 0,
        time_created integer, time_updated integer)"""

    def _db_path(self) -> str:
        con = sqlite3.connect(self.db_file)
        con.execute(self.SESSION_V2_DDL)
        con.execute(
            """CREATE TABLE session_message (
                id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
                seq integer NOT NULL, time_created integer NOT NULL,
                time_updated integer NOT NULL, data text NOT NULL)"""
        )
        con.execute(
            """CREATE TABLE event (
                aggregate_id text, type text, seq integer,
                data text, time_created integer)"""
        )
        con.execute(
            "INSERT INTO session_v2 (id, slug, title, agent, directory, parent_id,"
            " time_created, time_updated) VALUES (?, 's', 't', 'build', '/d', NULL,"
            " 1000, 1005)",
            (SESSION_ID,),
        )
        con.execute(
            "INSERT INTO session_message VALUES ('m1', ?, 'user', 1, 1001, 1001, ?)",
            (SESSION_ID, json.dumps({"time": {"created": 1001}, "text": "промпт"})),
        )
        con.execute(
            "INSERT INTO session_message VALUES ('m2', ?, 'synthetic', 2, 1002, 1002, ?)",
            (
                SESSION_ID,
                json.dumps({
                    "time": {"created": 1002},
                    "text": "The server restarted while you were working. "
                            "Continue from where you left off.",
                }),
            ),
        )
        con.execute(
            "INSERT INTO session_message VALUES ('m3', ?, 'assistant', 3, 1003, 1003, ?)",
            (SESSION_ID, json.dumps({
                "time": {"created": 1003},
                "content": [{"type": "text", "text": "продолжаю работу"}],
            })),
        )
        con.commit()
        con.close()
        return self.db_file

    def test_cli_session_dump_carries_synthetic_preview_in_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.db_file = str(Path(tmp) / "opencode.db")
            db = self._db_path()
            proc = subprocess.run(  # nosec B603 — argv константный, db-путь из tempfile теста
                [sys.executable, "tools/opencode_audit.py", "session", SESSION_ID,
                 "--db", db, "--compact"],
                capture_output=True, text=True, timeout=60,
            )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        dump = json.loads(proc.stdout)
        timeline = dump["timeline"]
        syn = [t for t in timeline if t["role"] == "synthetic"]
        self.assertEqual(len(syn), 1)
        self.assertIn("server restarted", syn[0]["preview"])
        # соседние роли не пострадали: user несёт свой текст, assistant — свой
        self.assertEqual(timeline[0]["preview"], "промпт")
        self.assertEqual(timeline[2]["preview"], "продолжаю работу")


if __name__ == "__main__":
    unittest.main()
