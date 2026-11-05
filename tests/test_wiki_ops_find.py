"""find обязан экранировать тайтл в SQL-литерале и печатать id + hpath найденных.

Контракты (задача 20261105195239, дедуп легаси-коллов):
  1) апостроф в тайтле удваивается в stmt (инъекция через свободный текст
     невозможна; живой класс дока «Хвосты amend'а merge-коммита»);
  2) найденные строки печатаются как «id hpath»;
  3) stmt несёт явный LIMIT (правило после orgls-инцидента).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_find.py
"""
import io
import re
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
