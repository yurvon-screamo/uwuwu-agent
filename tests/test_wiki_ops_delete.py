"""delete обязан сносить доки безопасно: guard'ы до removeDocByID, трейл, пост-чек.

Контракты (живая проверка форматов — на реальном ядре 05.11, задача 20261105181908):
  1) docId валидируется по формату SiYuan до ЛЮБОГО SQL (инъекция/мусор — отказ);
  2) несуществующий док — внятная ошибка, removeDocByID не вызывается;
  3) док с дочерними доками сносится только с --force (предикат детей — от path
     резолва без «.sy»: сегменты-директории в blocks.path не несут расширения);
  4) канон (без маркера заглушки) сносится только с --force; соседей считаем
     box-scoped по hpath; апостроф в hpath экранируется;
  5) маркер заглушки — строка-цитата с «⚠️» И словом «канон»/«заглушк» на той же
     строке (живой контрпример: цитата-упоминание без ⚠️ на канон-ранбуке);
  6) после ok от removeDocByID — подтверждение по индексу; док остался → громкая
     ошибка, тихого ok нет;
  7) каждый SQL-stmt несёт явный LIMIT (правило проекта после orgls-инцидента).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_delete.py
"""
import io
import re
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

BOX = "20261103224312-fbrf5fp"
CANON_ID = "20261105122006-zw94m4h"
STUB_ID = "20261105130411-fcemx1m"
BOARD_ID = "20261103233646-vzgad8m"
CHILD_ID = "20261105181908-1pm4db1"
APOSTROPHE_ID = "20261105181106-3kvzcuv"

STUB_CONTENT = (
    "# Дубль\n\n"
    "> ⚠️ **Дубль от гонки двух агентов** — канон **20261105122006-zw94m4h**; "
    "уникальный контент слит. Эта заглушка — удалить.\n"
)
CANON_WITH_QUOTE_MENTION = (
    "# Канон\n\n"
    "> Слит из одноимённого дубль-дока; дубль погашен заглушкой со ссылкой сюда.\n"
)


def _doc(doc_id: str, hpath: str, path: str, box: str = BOX) -> dict:
    return {"id": doc_id, "hpath": hpath, "path": path, "box": box, "type": "d"}


class FakeKernel:
    """Двойник kernel API: blocks-таблица, export, removeDocByID.

    SQL эмулируется по семантике stmt (id-резолв / дети по path-префиксу /
    box-scoped соседи), не по точной строке запроса.
    """

    def __init__(self, docs, contents=None, removal_updates_index=True):
        self.docs = {d["id"]: dict(d) for d in docs}
        self.contents = contents or {}
        self.removal_updates_index = removal_updates_index
        self.removed = []
        self.sql_stmts = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path == "/api/export/exportMdContent":
            return {"content": self.contents.get(body["id"], "")}
        if path == "/api/filetree/removeDocByID":
            self.removed.append(body["id"])
            if self.removal_updates_index:
                self.docs.pop(body["id"], None)
            return None
        raise AssertionError(f"неожиданный путь: {path}")

    def _sql(self, stmt: str):
        self.sql_stmts.append(stmt)
        rows = [dict(d) for d in self.docs.values() if d["type"] == "d"]
        m = re.search(r"path like '([^']*)/%'", stmt)
        if m:
            prefix = m.group(1) + "/"
            return [{"c": sum(1 for r in rows if r["path"].startswith(prefix))}]
        m_box = re.search(r"box='([^']+)'", stmt)
        m_hpath = re.search(r"hpath='((?:[^']|'')*)'", stmt)
        if m_box and m_hpath:
            hpath = m_hpath.group(1).replace("''", "'")
            return [{"c": sum(1 for r in rows
                              if r["box"] == m_box.group(1) and r["hpath"] == hpath)}]
        m_id = re.search(r"id='([^']+)'", stmt)
        if m_id:
            row = self.docs.get(m_id.group(1))
            return [dict(row)] if row else []
        raise AssertionError(f"непонятный stmt: {stmt}")


def run_delete(kernel: FakeKernel, doc_id: str, force: bool = False) -> str:
    args = SimpleNamespace(docId=doc_id, force=force)
    original_post = wiki_ops.post
    wiki_ops.post = kernel.post
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer), patch.object(wiki_ops.time, "sleep", lambda s: None):
            wiki_ops.cmd_delete(args)
    finally:
        wiki_ops.post = original_post
    return buffer.getvalue()


class StubDuplicateIsRemoved(unittest.TestCase):
    """Заглушка-дубль сносится без --force, трейл называет hpath и корзину."""

    def setUp(self):
        self.kernel = FakeKernel(
            docs=[_doc(CANON_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ.sy"),
                  _doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
        )

    def test_remove_called_with_stub_id(self):
        run_delete(self.kernel, STUB_ID)
        self.assertEqual(self.kernel.removed, [STUB_ID])

    def test_trail_names_path_and_destination(self):
        output = run_delete(self.kernel, STUB_ID)
        self.assertIn("/uwuwu/Ранбуки/РБ", output)
        self.assertIn("корзина", output)


class CanonIsProtected(unittest.TestCase):
    """Канон без маркера сносится только с --force."""

    def setUp(self):
        self.kernel = FakeKernel(
            docs=[_doc(CANON_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ.sy")],
            contents={CANON_ID: CANON_WITH_QUOTE_MENTION},
        )

    def test_refused_without_force(self):
        with self.assertRaises(RuntimeError) as cm:
            run_delete(self.kernel, CANON_ID)
        self.assertIn("--force", str(cm.exception))
        self.assertIn("канон", str(cm.exception).lower())
        self.assertEqual(self.kernel.removed, [])

    def test_force_removes_canon(self):
        run_delete(self.kernel, CANON_ID, force=True)
        self.assertEqual(self.kernel.removed, [CANON_ID])

    def test_quote_mention_without_warn_is_not_stub(self):
        # Живой контрпример: цитата-упоминание «заглушкой» без ⚠️ на строке —
        # классифицируется как канон, а не как заглушка.
        with self.assertRaises(RuntimeError):
            run_delete(self.kernel, CANON_ID)


class UnmarkedDuplicate(unittest.TestCase):
    """Дубль без маркера: отказ с подсказкой погасить заглушкой или --force."""

    def test_refused_with_hint(self):
        kernel = FakeKernel(
            docs=[_doc(CANON_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ.sy"),
                  _doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={},
        )
        with self.assertRaises(RuntimeError) as cm:
            run_delete(kernel, STUB_ID)
        self.assertIn("--force", str(cm.exception))
        self.assertEqual(kernel.removed, [])


class ChildrenGuard(unittest.TestCase):
    """Док с дочерними доками: subtree уходит только с --force."""

    def setUp(self):
        self.kernel = FakeKernel(
            docs=[_doc(BOARD_ID, "/uwuwu/Задачи uwuwu", "/20261103224612-e9q7eyv/Задачи uwuwu.sy"),
                  _doc(CHILD_ID, "/uwuwu/Задачи uwuwu/Тело",
                       "/20261103224612-e9q7eyv/Задачи uwuwu/20261105181908-1pm4db1.sy")],
        )

    def test_refused_without_force(self):
        with self.assertRaises(RuntimeError) as cm:
            run_delete(self.kernel, BOARD_ID)
        self.assertIn("дочерние доки (1)", str(cm.exception))
        self.assertEqual(self.kernel.removed, [])

    def test_force_removes_subtree_deliberately(self):
        run_delete(self.kernel, BOARD_ID, force=True)
        self.assertEqual(self.kernel.removed, [BOARD_ID])


class BadInputs(unittest.TestCase):
    """Мусорный docId и несуществующий док: отказ до сноса."""

    def test_malformed_doc_id_rejected_before_any_sql(self):
        kernel = FakeKernel(docs=[])
        with self.assertRaises(RuntimeError):
            run_delete(kernel, "x' or 1=1--")
        self.assertEqual(kernel.sql_stmts, [])
        self.assertEqual(kernel.removed, [])

    def test_missing_doc_fails_before_removal(self):
        kernel = FakeKernel(docs=[_doc(CANON_ID, "/uwuwu/РБ", "/20261103224612-e9q7eyv/РБ.sy")])
        with self.assertRaises(RuntimeError) as cm:
            run_delete(kernel, STUB_ID)
        self.assertIn("не найден", str(cm.exception))
        self.assertEqual(kernel.removed, [])


class PostCheck(unittest.TestCase):
    """ok от ядра при живом индексе — громкая ошибка, не тихий успех."""

    def test_doc_still_indexed_after_ok_is_loud_error(self):
        kernel = FakeKernel(
            docs=[_doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
            removal_updates_index=False,
        )
        with self.assertRaises(RuntimeError) as cm:
            run_delete(kernel, STUB_ID)
        self.assertIn("остался в индексе", str(cm.exception))
        self.assertEqual(kernel.removed, [STUB_ID])


class ApostropheInHpath(unittest.TestCase):
    """Апостроф в hpath экранируется в счётчике соседей (живой док «amend'а»)."""

    def test_sibling_count_query_escapes_quote(self):
        hpath = "/Experience/platform/Хвосты amend'а merge-коммита"
        kernel = FakeKernel(
            docs=[_doc(APOSTROPHE_ID, hpath, "/20261103224612-e9q7eyv/20261105181106-3kvzcuv.sy"),
                  _doc(CANON_ID, hpath, "/20261103224612-e9q7eyv/20261105122006-zw94m4h.sy")],
            contents={APOSTROPHE_ID: STUB_CONTENT},
        )
        output = run_delete(kernel, APOSTROPHE_ID)
        self.assertEqual(kernel.removed, [APOSTROPHE_ID])
        self.assertTrue(any("''" in s for s in kernel.sql_stmts),
                        f"hpath без экранирования кавычки: {kernel.sql_stmts}")
        self.assertIn("соседей по пути: 2", output)


class SqlDiscipline(unittest.TestCase):
    """Каждый stmt delete несёт явный LIMIT (правило проекта после orgls)."""

    def test_every_sql_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(
            docs=[_doc(CANON_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ.sy"),
                  _doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
        )
        run_delete(kernel, STUB_ID)
        self.assertTrue(kernel.sql_stmts, "delete не выполнил ни одного sql-запроса")
        for stmt in kernel.sql_stmts:
            self.assertIn(" limit 1", stmt, f"stmt без явного LIMIT: {stmt}")


if __name__ == "__main__":
    unittest.main()
