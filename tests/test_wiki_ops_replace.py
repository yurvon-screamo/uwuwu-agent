"""replace обязан валидировать docId до чтения файла и любых SQL, не терять док.

Контракты (паттерн delete-валидации, задача 20261105195239):
  1) docId валидируется по формату SiYuan ДО чтения --md файла и любого SQL
     (мусорный/инъекционный id — отказ «неверный docId», файл может отсутствовать);
  2) несуществующий док — внятная ошибка, removeDocByID/createDocWithMd не зовутся;
  3) happy path: док сносится и пересоздаётся на том же hpath в том же ноутбуке
     с содержимым файла; resolve-stmt несёт явный LIMIT (правило после orgls).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_replace.py
"""
import io
import re
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

BOX = "20261103224312-fbrf5fp"
NB_PROJECTS = {"id": BOX, "name": "Projects", "closed": False}
DOC_ID = "20261105122006-zw94m4h"
DOC = {"id": DOC_ID, "hpath": "/uwuwu/Ранбуки/РБ", "box": BOX, "type": "d"}


class FakeKernel:
    """Двойник kernel API для replace: id-резолв, ноутбуки, remove/create."""

    def __init__(self, docs, notebooks=(NB_PROJECTS,)):
        self.docs = {d["id"]: dict(d) for d in docs}
        self.notebooks = list(notebooks)
        self.sql_stmts = []
        self.removed = []
        self.created = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path == "/api/notebook/lsNotebooks":
            return {"notebooks": [dict(nb) for nb in self.notebooks]}
        if path == "/api/filetree/removeDocByID":
            self.removed.append(body["id"])
            return None
        if path == "/api/filetree/createDocWithMd":
            self.created.append({"notebook": body["notebook"], "path": body["path"],
                                 "markdown": body["markdown"]})
            return None
        raise AssertionError(f"неожиданный путь: {path}")

    def _sql(self, stmt: str):
        self.sql_stmts.append(stmt)
        m = re.search(r"id='([^']+)'", stmt)
        if m:
            row = self.docs.get(m.group(1))
            return [dict(row)] if row else []
        raise AssertionError(f"непонятный stmt: {stmt}")


def run_replace(kernel: FakeKernel, doc_id: str, file: str) -> str:
    args = SimpleNamespace(docId=doc_id, file=file)
    original_post = wiki_ops.post
    wiki_ops.post = kernel.post
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer), patch.object(wiki_ops.time, "sleep", lambda s: None):
            wiki_ops.cmd_replace(args)
    finally:
        wiki_ops.post = original_post
    return buffer.getvalue()


class MalformedDocId(unittest.TestCase):
    """Мусорный docId: отказ до чтения файла и любых SQL."""

    def test_rejected_before_file_read_and_any_sql(self):
        kernel = FakeKernel(docs=[])
        with tempfile.TemporaryDirectory() as tmp:
            missing = str(Path(tmp) / "нет-такого-файла.md")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, "x' or 1=1--", missing)
        self.assertIn("неверный docId", str(cm.exception))
        self.assertEqual(kernel.sql_stmts, [])
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])


class UnknownDoc(unittest.TestCase):
    """Валидный docId без дока в индексе: отказ до сноса."""

    def test_missing_doc_fails_before_removal(self):
        kernel = FakeKernel(docs=[])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("# Контент\n")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, DOC_ID, str(md))
        self.assertIn("не найден", str(cm.exception))
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])


class HappyPath(unittest.TestCase):
    """Док сносится и пересоздаётся на том же hpath с контентом файла."""

    def test_recreated_in_same_notebook_and_path(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n\nТело.\n")
            output = run_replace(kernel, DOC_ID, str(md))
        self.assertEqual(kernel.removed, [DOC_ID])
        self.assertEqual(len(kernel.created), 1)
        created = kernel.created[0]
        self.assertEqual(created["notebook"], BOX)
        self.assertEqual(created["path"], "/uwuwu/Ранбуки/РБ")
        self.assertIn("# Новый канон", created["markdown"])
        self.assertIn("/uwuwu/Ранбуки/РБ", output)

    def test_resolve_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("# x\n")
            run_replace(kernel, DOC_ID, str(md))
        self.assertTrue(kernel.sql_stmts, "replace не выполнил ни одного sql-запроса")
        for stmt in kernel.sql_stmts:
            self.assertIn(" limit 1", stmt, f"stmt без явного LIMIT: {stmt}")


if __name__ == "__main__":
    unittest.main()
