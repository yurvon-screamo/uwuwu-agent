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
  6) после ok от removeDocByID вердикт — по файловой пробе ядра (FS-истина:
     getFile data/<box><path>, контракт 3.8.9); файл остался на диске → громкая
     ошибка, тихого ok без вердикта нет; SQL-индекс (лагает секундами после
     мутаций) — примечание к успеху или фолбэк при failed-пробе, успех не
     отвергает (принцип ADR-025: один лагающий источник ≠ истина);
  7) каждый SQL-stmt несёт явный LIMIT (правило проекта после orgls-инцидента).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_delete.py
"""
import io
import json
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
    """Двойник kernel API: blocks-таблица, export, removeDocByID, getFile-проба.

    SQL эмулируется по семантике stmt (id-резолв / дети по path-префиксу /
    box-scoped соседи), не по точной строке запроса. FS — отдельная модель
    (set путей data<path>): файл уходит при removeDocByID независимо от
    SQL-индекса, fs_probe различает found/missing/failed как ядро 3.8.9.
    """

    def __init__(self, docs, contents=None, removal_updates_index=True, notebooks=None,
                 removal_updates_fs=True, fs_probe_fails=False):
        self.docs = {d["id"]: dict(d) for d in docs}
        self.contents = contents or {}
        self.removal_updates_index = removal_updates_index
        self.removal_updates_fs = removal_updates_fs
        self.fs_probe_fails = fs_probe_fails
        self.notebooks = notebooks
        self.fs = {f"data{d['path']}" for d in self.docs.values()}
        self.removed = []
        self.sql_stmts = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path == "/api/notebook/lsNotebooks":
            if self.notebooks is None:
                raise RuntimeError("POST /api/notebook/lsNotebooks: ядро недоступно")
            return {"notebooks": [dict(nb) for nb in self.notebooks]}
        if path == "/api/export/exportMdContent":
            return {"content": self.contents.get(body["id"], "")}
        if path == "/api/filetree/removeDocByID":
            self.removed.append(body["id"])
            doc = self.docs.get(body["id"])
            if self.removal_updates_index:
                self.docs.pop(body["id"], None)
            if self.removal_updates_fs and doc:
                self.fs.discard(f"data{doc['path']}")
            return None
        raise AssertionError(f"неожиданный путь: {path}")

    def fs_probe(self, path: str) -> str:
        """getFile-семантика ядра 3.8.9: 200+байты → found, конверт 404 → missing,
        отказ пробы → failed (флаг fs_probe_fails)."""
        if self.fs_probe_fails:
            return "failed"
        return "found" if path in self.fs else "missing"

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
    original_fs_file = getattr(wiki_ops, "_fs_file", None)
    wiki_ops.post = kernel.post
    wiki_ops._fs_file = kernel.fs_probe
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer), patch.object(wiki_ops.time, "sleep", lambda s: None):
            wiki_ops.cmd_delete(args)
    finally:
        wiki_ops.post = original_post
        if original_fs_file is not None:
            wiki_ops._fs_file = original_fs_file
        else:
            del wiki_ops._fs_file
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
    """Вердикт после ok — по файловой пробе (FS-истина); лагающий индекс
    успех не отвергает, файл на диске — громкая ошибка."""

    def _kernel(self, **kwargs):
        return FakeKernel(
            docs=[_doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
            **kwargs,
        )

    def test_index_lag_after_ok_is_quiet_success_with_fs_verdict(self):
        # Красный тест задачи: removeDocByID ok, файл уехал в корзину,
        # SQL-индекс ещё показывает док — тихий успех, никакого raise.
        kernel = self._kernel(removal_updates_index=False)
        output = run_delete(kernel, STUB_ID)
        self.assertEqual(kernel.removed, [STUB_ID])
        self.assertIn("снесён", output)
        self.assertIn("вердикт по файловой пробе", output)
        self.assertIn("SQL-индекс ещё видит док", output)

    def test_file_left_on_disk_is_loud_error_with_path(self):
        kernel = self._kernel(removal_updates_fs=False)
        with self.assertRaises(RuntimeError) as cm:
            run_delete(kernel, STUB_ID)
        message = str(cm.exception)
        self.assertIn("файл дока остался на диске", message)
        self.assertIn("data/20261103224612-e9q7eyv/РБ/РБ.sy", message)
        self.assertEqual(kernel.removed, [STUB_ID])

    def test_fs_probe_failure_falls_back_to_index_verdict(self):
        kernel = self._kernel(fs_probe_fails=True)
        output = run_delete(kernel, STUB_ID)
        self.assertEqual(kernel.removed, [STUB_ID])
        self.assertIn("снесён", output)
        self.assertIn("вердикт по SQL-индексу (файловая проба не состоялась)", output)

    def test_fs_probe_failure_with_stale_index_is_loud_error(self):
        kernel = self._kernel(removal_updates_index=False, fs_probe_fails=True)
        with self.assertRaises(RuntimeError) as cm:
            run_delete(kernel, STUB_ID)
        self.assertIn("верификация не сошлась", str(cm.exception))
        self.assertEqual(kernel.removed, [STUB_ID])


class _FakeResponse:
    """Стенд urlopen-ответа: status + тело, контекстный менеджер как у urllib."""

    def __init__(self, status: int, body: bytes):
        self.status = status
        self._body = body

    def read(self) -> bytes:
        return self._body

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FsProbeContract(unittest.TestCase):
    """Транспортный контракт _fs_file пинится целиком (юнит-тесты cmd_delete
    патчат _fs_file — здесь проверяется сам выстрел): Authorization из _conf,
    явный timeout, путь в теле, три исхода ядра 3.8.9."""

    KERNEL_URL = "http://kernel.test"
    DOC_PATH = "data/20261103224612-e9q7eyv/РБ/РБ.sy"

    def _probe_with(self, response):
        captured = {}

        def fake_urlopen(req, timeout=None):
            captured["auth"] = req.get_header("Authorization")
            captured["timeout"] = timeout
            captured["body"] = json.loads(req.data.decode())
            if isinstance(response, Exception):
                raise response
            return response

        saved = (wiki_ops.URL, wiki_ops.TOKEN)
        wiki_ops.URL, wiki_ops.TOKEN = self.KERNEL_URL, "t0k3n"
        try:
            with patch.object(wiki_ops.urllib.request, "urlopen", fake_urlopen):
                verdict = wiki_ops._fs_file(self.DOC_PATH)
        finally:
            wiki_ops.URL, wiki_ops.TOKEN = saved
        return verdict, captured

    def test_sends_auth_token_and_explicit_timeout(self):
        verdict, captured = self._probe_with(_FakeResponse(200, b'{"ID":"doc"}'))
        self.assertEqual(verdict, "found")
        self.assertEqual(captured["auth"], "Token t0k3n")
        self.assertEqual(captured["timeout"], 60)
        self.assertEqual(captured["body"], {"path": self.DOC_PATH})

    def test_http_202_envelope_code_404_is_missing(self):
        body = b'{"code":404,"msg":"open \u2026: file does not exist","data":null}'
        verdict, _ = self._probe_with(_FakeResponse(202, body))
        self.assertEqual(verdict, "missing")

    def test_http_200_with_empty_body_is_failed_not_found(self):
        # Пустое тело это промах, не успех (правило write-path, ловушка blob).
        verdict, _ = self._probe_with(_FakeResponse(200, b""))
        self.assertEqual(verdict, "failed")

    def test_unexpected_envelope_code_is_failed(self):
        verdict, _ = self._probe_with(_FakeResponse(202, b'{"code":-1,"msg":"wave"}'))
        self.assertEqual(verdict, "failed")

    def test_transport_error_is_failed(self):
        verdict, _ = self._probe_with(Exception("connection refused"))
        self.assertEqual(verdict, "failed")


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


class TrailNotebookName(unittest.TestCase):
    """Трейл называет человекочитаемое имя ноутбука; отказ lsNotebooks —
    фолбэк на box-id, исход успешного сноса не меняется."""

    def test_trail_prints_notebook_name(self):
        kernel = FakeKernel(
            docs=[_doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
            notebooks=[{"id": BOX, "name": "Projects", "closed": False}],
        )
        output = run_delete(kernel, STUB_ID)
        self.assertIn("(ноутбук Projects)", output)

    def test_notebooks_failure_falls_back_to_box_id_without_failing_command(self):
        kernel = FakeKernel(
            docs=[_doc(STUB_ID, "/uwuwu/Ранбуки/РБ", "/20261103224612-e9q7eyv/РБ/РБ.sy")],
            contents={STUB_ID: STUB_CONTENT},
            notebooks=None,
        )
        output = run_delete(kernel, STUB_ID)
        self.assertIn(f"(ноутбук {BOX})", output)
        self.assertEqual(kernel.removed, [STUB_ID])


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
