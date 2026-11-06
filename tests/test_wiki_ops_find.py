"""find обязан экранировать тайтл в SQL-литерале и печатать id + hpath найденных.

Контракты (задача 20261105195239, дедуп легаси-коллов):
  1) апостроф в тайтле удваивается в stmt (инъекция через свободный текст
     невозможна; живой класс дока «Хвосты amend'а merge-коммита»);
  2) найденные строки печатаются как «id hpath»;
  3) stmt несёт явный LIMIT (правило после orgls-инцидента).

Контракты сбоя (задача 20261104072714 — «сбой маскируется под "не найден"»):
  4) отказ ядра (code≠0) → «ERROR: …» с причиной, exit 1, stdout пуст —
     текст ошибки никогда не выглядит как «не найдено»;
  5) честный not-found на здоровом ядре — тихий exit 0 (конвенция
     «успех — тихий»), сбой от него отличим по паре exit-код/stderr.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_find.py
"""
import io
import re
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

DOC_ID = "20261105181106-3kvzcuv"
DOC = {"id": DOC_ID, "hpath": "/Experience/platform/Хвосты amend'а merge-коммита",
       "content": "тело с упоминанием amend'а"}
OTHER = {"id": "20261105122006-zw94m4h", "hpath": "/uwuwu/Ранбуки/РБ", "content": "другое"}


class FakeKernel:
    """Двойник kernel API: эмуляция content like '%…%' по семантике stmt."""

    def __init__(self, docs):
        self.docs = docs
        self.sql_stmts = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            self.sql_stmts.append(body["stmt"])
            m = re.search(r"content like '%(.*?)%' limit", body["stmt"])
            needle = m.group(1).replace("''", "'")
            return [dict(d) for d in self.docs if needle in d.get("content", "")]
        raise AssertionError(f"неожиданный путь: {path}")


def run_find(kernel: FakeKernel, title: str) -> str:
    args = type("Args", (), {"title": title})()
    original_post = wiki_ops.post
    wiki_ops.post = kernel.post
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer):
            wiki_ops.cmd_find(args)
    finally:
        wiki_ops.post = original_post
    return buffer.getvalue()


class TitleEscaping(unittest.TestCase):
    """Апостроф в тайтле экранируется удвоением в stmt."""

    def test_apostrophe_is_doubled_in_stmt(self):
        kernel = FakeKernel(docs=[DOC])
        run_find(kernel, "amend'а")
        self.assertTrue(any("amend''а" in s for s in kernel.sql_stmts),
                        f"тайтл без экранирования кавычки: {kernel.sql_stmts}")

    def test_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(docs=[DOC])
        run_find(kernel, "аменд")
        for stmt in kernel.sql_stmts:
            self.assertIn(" limit 10", stmt, f"stmt без явного LIMIT: {stmt}")


class Output(unittest.TestCase):
    """Найденные строки печатаются как id + hpath, ненайденные — молчание."""

    def test_found_rows_printed_as_id_and_hpath(self):
        kernel = FakeKernel(docs=[DOC, OTHER])
        output = run_find(kernel, "amend'а")
        self.assertIn(f"{DOC_ID} /Experience/platform/Хвосты amend'а merge-коммита", output)
        self.assertNotIn(OTHER["id"], output)

    def test_no_match_prints_nothing(self):
        kernel = FakeKernel(docs=[OTHER])
        output = run_find(kernel, "несуществующий-тайтл")
        self.assertEqual(output, "")


def refused_kernel(path, body, attempts=6):
    raise wiki_ops.KernelRefused("POST /api/query/sql: code=500 msg=boom")


class FailureIsNotNotFound(unittest.TestCase):
    """Отказ ядра не проходит за «не найдено»: exit 1 + причина в stderr."""

    def test_kernel_refusal_prints_error_and_exits_1(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "find", "Стек"]), \
                patch.object(wiki_ops, "post", refused_kernel), \
                redirect_stdout(out), redirect_stderr(err):
            with self.assertRaises(SystemExit) as cm:
                wiki_ops.main()
        self.assertEqual(cm.exception.code, 1)
        self.assertIn("ERROR:", err.getvalue())
        self.assertIn("code=500", err.getvalue(), "последняя причина не дошла до ошибки")
        self.assertNotIn("не найден", err.getvalue())
        self.assertNotIn("не найдено", err.getvalue())
        self.assertEqual(out.getvalue(), "")

    def test_honest_not_found_is_quiet_exit_0(self):
        kernel = FakeKernel(docs=[])
        args = type("Args", (), {"title": "несуществующий-тайтл"})()
        out = io.StringIO()
        with patch.object(wiki_ops, "post", kernel.post), redirect_stdout(out):
            wiki_ops.cmd_find(args)  # ни исключения, ни SystemExit — естественный 0
        self.assertEqual(out.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
