"""export-поверхность не отвечает пустым успехом на аномальный конверт ядра.

Контракт (задача 20261107033538 — граница класса PR #111):
  1) конверт /api/export/exportMdContent обязан нести dict со str-полем
     "content": всё остальное (None, dict без поля, content не строка) —
     RuntimeError с именем эндпоинта; голая «(r or {}).get("content", "")»
     читала бы сбой как «док пуст» + exit 0;
  2) пустой content при здоровом конверте легитимен — ядро отвечает им и на
     пустой док, и на несуществующий (проба 07.11: code=0,
     data={hPath: '', content: ''}): конвенция «успех — тихий» = честный
     not-found, регресса на ней нет;
  3) ретраи: командные читатели (cmd_export, _is_stub) — пост-дефолт 6,
     guard-шаги append/replace — single-shot (attempts=1), бюджет держит
     вызывающий луп;
  4) main-wrapper: аномалия → exit 1, «ERROR: …exportMdContent…» без
     трейсбека, stdout пуст.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_export.py
"""
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops
from tests.wiki_ops_testkit import kernel_post, run_cmd

DOC = "20261107000000-abcdefg"
EXPORT_MD_PATH = "/api/export/exportMdContent"


class ExportKernel:
    """Двойник export/appendBlock-поверхности: конверт payload отдаётся на
    exportMdContent, вызовы (путь, attempts) записываются."""

    def __init__(self, payload):
        self.payload = payload
        self.calls = []

    def post(self, path, body, attempts=6):
        self.calls.append((path, attempts))
        if path == EXPORT_MD_PATH:
            return self.payload
        return None  # appendBlock — успех без data


def post_returning(payload):
    def post(path, body, attempts=6):
        if path != EXPORT_MD_PATH:
            raise AssertionError(f"неожиданный путь: {path}")
        return payload
    return post


class EnvelopeDiscipline(unittest.TestCase):
    """Аномальные конверты exportMdContent — громкая ошибка, не пустая строка."""

    def _export_raises(self, payload):
        with self.assertRaises(RuntimeError) as cm:
            with kernel_post(post_returning(payload)):
                wiki_ops._export_content(DOC)
        return cm.exception

    def test_none_payload_raises_not_silent_empty(self):
        err = self._export_raises(None)
        self.assertIn("exportMdContent вернул аномальный конверт", str(err))

    def test_dict_without_content_raises(self):
        err = self._export_raises({"hPath": "/x"})
        self.assertIn("аномальный конверт", str(err))

    def test_non_string_content_raises(self):
        err = self._export_raises({"content": 42})
        self.assertIn("аномальный конверт", str(err))

    def test_healthy_envelope_content_returned(self):
        with kernel_post(post_returning({"hPath": "/uwuwu/x", "content": "текст\n"})):
            self.assertEqual(wiki_ops._export_content(DOC), "текст\n")


class LegitimateEmpty(unittest.TestCase):
    """Пустой content при здоровом конверте — честный not-found, не сбой."""

    def test_empty_content_is_returned_not_refused(self):
        with kernel_post(post_returning({"hPath": "", "content": ""})):
            self.assertEqual(wiki_ops._export_content(DOC), "")

    def test_export_command_prints_empty_and_stays_quiet(self):
        args = type("Args", (), {"docId": DOC})()
        out, err = run_cmd(wiki_ops.cmd_export, args, ExportKernel({"hPath": "", "content": ""}))
        self.assertEqual(out, "\n")  # print("") — только перевод строки
        self.assertEqual(err, "")


class StubDetection(unittest.TestCase):
    """_is_stub: аномалия громкая; легитимная пустота — не заглушка."""

    def test_anomalous_envelope_raises_not_silent_false(self):
        with self.assertRaises(RuntimeError) as cm:
            with kernel_post(post_returning(None)):
                wiki_ops._is_stub(DOC)
        self.assertIn("exportMdContent", str(cm.exception))

    def test_empty_content_is_not_stub(self):
        with kernel_post(post_returning({"content": ""})):
            self.assertFalse(wiki_ops._is_stub(DOC))

    def test_stub_marker_detected(self):
        marker = "> ⚠️ Заглушка дубля. Канон — 20261106000000-xxxxxxx\n"
        with kernel_post(post_returning({"content": marker})):
            self.assertTrue(wiki_ops._is_stub(DOC))


class AttemptsSemantics(unittest.TestCase):
    """Ретраи export-чтения: команды — 6, guard-шаги append/replace — 1."""

    def test_export_command_reads_with_default_retries(self):
        kernel = ExportKernel({"content": "x"})
        args = type("Args", (), {"docId": DOC})()
        run_cmd(wiki_ops.cmd_export, args, kernel)
        self.assertEqual(kernel.calls, [(EXPORT_MD_PATH, 6)])

    def test_is_stub_reads_with_default_retries(self):
        kernel = ExportKernel({"content": ""})
        with kernel_post(kernel.post):
            wiki_ops._is_stub(DOC)
        self.assertEqual(kernel.calls, [(EXPORT_MD_PATH, 6)])

    def test_append_guard_step_is_single_shot(self):
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "append.md"
            md.write_text("новый хвост\n")
            kernel = ExportKernel({"content": "Базовый параграф.\n"})
            args = type("Args", (), {"docId": DOC, "file": str(md), "force": False})()
            run_cmd(wiki_ops.cmd_append, args, kernel)
        export_calls = [c for c in kernel.calls if c[0] == EXPORT_MD_PATH]
        self.assertEqual(export_calls, [(EXPORT_MD_PATH, 1)])

    def test_replace_freshness_guard_step_is_single_shot(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "snapshot.md"
            base.write_text("устаревший снапшот\n")
            kernel = ExportKernel({"content": "свежий контент\n"})
            with kernel_post(kernel.post), self.assertRaises(RuntimeError) as cm:
                wiki_ops._require_fresh_snapshot(DOC, str(base))
        export_calls = [c for c in kernel.calls if c[0] == EXPORT_MD_PATH]
        self.assertEqual(export_calls, [(EXPORT_MD_PATH, 1)])
        self.assertIn("менялся после снапшота", str(cm.exception))


class MainWrapperContract(unittest.TestCase):
    """Аномалия export через main: exit 1, ERROR с именем эндпоинта, без трейсбека."""

    def test_anomalous_envelope_exits_1_with_endpoint_named(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "export", DOC]), \
                patch.object(wiki_ops, "post", post_returning(None)), \
                redirect_stdout(out), redirect_stderr(err):
            with self.assertRaises(SystemExit) as cm:
                wiki_ops.main()
        self.assertEqual(cm.exception.code, 1)
        self.assertIn("ERROR: exportMdContent вернул аномальный конверт", err.getvalue())
        self.assertNotIn("Traceback", err.getvalue())
        self.assertEqual(out.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
