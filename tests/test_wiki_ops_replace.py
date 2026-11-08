"""replace обязан валидировать docId до чтения файла и любых SQL, не терять док
и не затирать чужие правки, сделанные после снапшота (guard lost-update).

Контракты (паттерн delete-валидации, задача 20261105195239; guard — инцидент
05.11: rewrite «Стек и архитектура» затёр параллельный append другой сессии):
  1) docId валидируется по формату SiYuan ДО чтения --md файла и любого SQL
     (мусорный/инъекционный id — отказ «неверный docId», файл может отсутствовать);
  2) несуществующий док — внятная ошибка, removeDocByID/createDocWithMd не зовутся;
  3) happy path: док сносится и пересоздаётся на том же hpath в том же ноутбуке
     с содержимым файла; resolve-stmt несёт явный LIMIT (правило после orgls);
  4) guard свежести: без --base и без --force — отказ с рецептом до первой
     мутации; протухший снапшот (контент ядра менялся после снятия base) —
     отказ до removeDocByID; свежий снапшот — replace проходит; --force старше
     --base (осознанный overwrite без сверок, как append --force); нормализация
     гасит рендер-шум ядра; сбой export для сверки — громкая ошибка, мутаций нет;
  5) payload сырым export'ом (frontmatter + ведущий title-H1) стрипается до
     ядра — «правь копию export» не портит док (инцидент 08.11); payload,
     пустой после стрипа, — отказ до guard'а и сноса; гард --base со
     снапшотом в экспорт-форме жив со стрипом payload'а;
  6) вывод несёт НОВЫЙ docId (replace пересоздаёт док, старый id мёртв —
     держатель иначе ретраит мёртвый, инцидент 08.11): первичный источник —
     data createDocWithMd, фолбэк getIDsByHPath; не резолвился — warning с
     рецептом find, rc 0 (мутация применена).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_replace.py
"""
import re
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops
from tests.wiki_ops_testkit import run_cmd

BOX = "20261103224312-fbrf5fp"
NB_PROJECTS = {"id": BOX, "name": "Projects", "closed": False}
DOC_ID = "20261105122006-zw94m4h"
DOC = {"id": DOC_ID, "hpath": "/uwuwu/Ранбуки/РБ", "box": BOX, "type": "d"}

# Форма живого export: frontmatter несёт lastmod (= blocks.updated, пробы 06.11)
BASE_CONTENT = ("---\ntitle: РБ\ndate: 2026-11-05T12:20:06+08:00\n"
                "lastmod: 2026-11-05T14:00:00+08:00\n---\n\n# РБ\n\nТело.\n")


class FakeKernel:
    """Двойник kernel API для replace: id-резолв, ноутбуки, export, remove/create.

    create_returns — data ответа createDocWithMd (живое ядро возвращает docId
    строкой, проба 08.11); hpath_ids — массив ответа getIDsByHPath (фолбэк).
    """

    DEFAULT_ID = "20261108000000-repla00"

    def __init__(self, docs, content=BASE_CONTENT, notebooks=(NB_PROJECTS,),
                 create_returns: object = DEFAULT_ID,
                 hpath_ids: list[str] | None = None):
        self.docs = {d["id"]: dict(d) for d in docs}
        self.content = content
        self.notebooks = list(notebooks)
        self.sql_stmts = []
        self.removed = []
        self.created = []
        self.export_calls = 0
        self.export_attempts = []
        self.create_returns = create_returns
        self.hpath_ids = hpath_ids or []
        self.calls: list[tuple[str, dict]] = []

    def post(self, path: str, body: dict, attempts: int = 6):
        self.calls.append((path, body))
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path == "/api/notebook/lsNotebooks":
            return {"notebooks": [dict(nb) for nb in self.notebooks]}
        if path == wiki_ops.EXPORT_MD_PATH:
            self.export_calls += 1
            self.export_attempts.append(attempts)
            return {"content": self.content}
        if path == "/api/filetree/removeDocByID":
            self.removed.append(body["id"])
            return None
        if path == "/api/filetree/createDocWithMd":
            self.created.append({"notebook": body["notebook"], "path": body["path"],
                                 "markdown": body["markdown"]})
            return self.create_returns
        if path == "/api/filetree/getIDsByHPath":
            return list(self.hpath_ids)
        raise AssertionError(f"неожиданный путь: {path}")

    def _sql(self, stmt: str):
        self.sql_stmts.append(stmt)
        m = re.search(r"id='([^']+)'", stmt)
        if m:
            row = self.docs.get(m.group(1))
            return [dict(row)] if row else []
        raise AssertionError(f"непонятный stmt: {stmt}")


def run_replace(kernel: FakeKernel, doc_id: str, file: str,
                base: str | None = None, force: bool = False) -> str:
    args = SimpleNamespace(docId=doc_id, file=file, base=base, force=force)
    with patch.object(wiki_ops.time, "sleep", lambda s: None):
        out, _ = run_cmd(wiki_ops.cmd_replace, args, kernel)
    return out


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
    """Валидный docId без дока в индексе: отказ до guard'а и сноса."""

    def test_missing_doc_fails_before_removal(self):
        kernel = FakeKernel(docs=[])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("# Контент\n")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, DOC_ID, str(md), base="снапшот-существует")
        self.assertIn("не найден", str(cm.exception))
        self.assertEqual(kernel.export_calls, 0)
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])


class SnapshotRequired(unittest.TestCase):
    """Без --base и без --force: отказ с рецептом до первой мутации."""

    def test_replace_without_snapshot_refused_before_removal(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, DOC_ID, str(md))
        self.assertIn("--base", str(cm.exception))
        self.assertIn("--force", str(cm.exception))
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])


class StaleSnapshot(unittest.TestCase):
    """Регресс инцидента 05.11: append другой сессии после снятия снапшота —
    rewrite отказывается до removeDocByID, чужой блок не затёрт."""

    def test_append_after_snapshot_refuses_replace(self):
        content_after_append = BASE_CONTENT + "\n\nУточнение чужой сессии.\n"
        kernel = FakeKernel(docs=[DOC], content=content_after_append)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "новый.md"
            md.write_text("# Переписанный канон\n")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertIn("менялся после снапшота", str(cm.exception))
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])

    def test_fresh_snapshot_recreated_in_same_notebook_and_path(self):
        kernel = FakeKernel(docs=[DOC], content=BASE_CONTENT)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n\nТело.\n")
            output = run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertEqual(kernel.removed, [DOC_ID])
        self.assertEqual(len(kernel.created), 1)
        created = kernel.created[0]
        self.assertEqual(created["notebook"], BOX)
        self.assertEqual(created["path"], "/uwuwu/Ранбуки/РБ")
        self.assertIn("# Новый канон", created["markdown"])
        self.assertIn("/uwuwu/Ранбуки/РБ", output)

    def test_render_noise_does_not_stale_the_snapshot(self):
        # Нормализация гасит рендер-шум ядра (хвостовые пробелы строки — strip,
        # пустые строки вне fence — дроп): дрейф только нерендеримых различий —
        # снапшот считается свежим.
        noisy = BASE_CONTENT.replace("# РБ\n\nТело.", "# РБ  \n\n\nТело.")
        kernel = FakeKernel(docs=[DOC], content=noisy)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n")
            run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertEqual(kernel.removed, [DOC_ID])
        self.assertEqual(len(kernel.created), 1)


class ForceOverridesBase(unittest.TestCase):
    """--force старше --base: осознанный overwrite идёт без guard'а и без export."""

    def test_force_bypasses_guard_even_with_stale_snapshot(self):
        content_after_append = BASE_CONTENT + "\n\nЧужое.\n"
        kernel = FakeKernel(docs=[DOC], content=content_after_append)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# Переписанный канон\n")
            output = run_replace(kernel, DOC_ID, str(md), base=str(base), force=True)
        self.assertEqual(kernel.export_calls, 0, "--force не должен снимать export для сверки")
        self.assertEqual(kernel.removed, [DOC_ID])
        self.assertEqual(len(kernel.created), 1)
        self.assertIn("заменён", output)


class GuardReadFailure(unittest.TestCase):
    """Export для сверки не читается: громкая ошибка, мутаций нет — не тихий
    пропуск guard'а."""

    def test_unreadable_export_fails_loud_before_removal(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n")
            with patch.object(wiki_ops, "_export_content",
                              side_effect=RuntimeError("POST export: network down")):
                with self.assertRaises(RuntimeError) as cm:
                    run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertIn("guard свежести не состоялся", str(cm.exception))
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])

    def test_guard_export_is_single_shot(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# Новый канон\n")
            run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertEqual(kernel.export_attempts, [1], "guard-экспорт идёт attempts=1 (write-path)")


class ResolveStmtLimit(unittest.TestCase):
    """Каждый stmt несёт явный LIMIT (правило после orgls)."""

    def test_resolve_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("# x\n")
            run_replace(kernel, DOC_ID, str(md), base=str(base))
        self.assertTrue(kernel.sql_stmts, "replace не выполнил ни одного sql-запроса")
        for stmt in kernel.sql_stmts:
            self.assertIn(" limit 1", stmt, f"stmt без явного LIMIT: {stmt}")


class ExportWrapperAndDocId(unittest.TestCase):
    """Payload сырым export'ом не портит док (инцидент 08.11); вывод несёт
    НОВЫЙ docId — replace пересоздаёт док, старый id мёртв."""

    def test_raw_export_payload_is_stripped_before_recreate(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text(BASE_CONTENT)  # «правь копию export» без правок
            with patch.object(wiki_ops.time, "sleep", lambda s: None):
                out, err = run_cmd(wiki_ops.cmd_replace,
                                   SimpleNamespace(docId=DOC_ID, file=str(md),
                                                   base=str(base), force=False),
                                   kernel)
        self.assertEqual(kernel.created[0]["markdown"], "Тело.\n")
        self.assertIn("снята экспорт-обвязка", err)
        self.assertIn("новый docId", out)
        self.assertIn(FakeKernel.DEFAULT_ID, out)
        self.assertIn(f"старый {DOC_ID} мёртв", out)

    def test_create_response_id_used_before_fallback(self):
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("Тело.\n")
            with patch.object(wiki_ops.time, "sleep", lambda s: None):
                run_cmd(wiki_ops.cmd_replace,
                        SimpleNamespace(docId=DOC_ID, file=str(md),
                                        base=str(base), force=False),
                        kernel)
        self.assertEqual([p for p, _ in kernel.calls
                          if p == "/api/filetree/getIDsByHPath"], [],
                         "строка в data createDocWithMd — фолбэк не зовётся")

    def test_unresolved_new_id_warns_and_replacement_stays(self):
        kernel = FakeKernel(docs=[DOC], create_returns=None, hpath_ids=[])
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("Тело.\n")
            with patch.object(wiki_ops.time, "sleep", lambda s: None):
                out, err = run_cmd(wiki_ops.cmd_replace,
                                   SimpleNamespace(docId=DOC_ID, file=str(md),
                                                   base=str(base), force=False),
                                   kernel)
        self.assertIn("заменён:", out)  # мутация применена — rc 0
        self.assertIn("ПРЕДУПРЕЖДЕНИЕ", err)
        self.assertIn('wiki_ops find "РБ"', err)

    def test_empty_after_strip_refused_before_guard_and_removal(self):
        # «Один export без правок» минус обвязка — пусто: отказ до guard'а и
        # removeDocByID (снос живого дока ради пустого — недопустим).
        kernel = FakeKernel(docs=[DOC])
        with tempfile.TemporaryDirectory() as tmp:
            md = Path(tmp) / "док.md"
            md.write_text("---\ntitle: РБ\n---\n")
            with self.assertRaises(RuntimeError) as cm:
                run_replace(kernel, DOC_ID, str(md), base="снапшот")
        self.assertIn("пуст после снятия экспорт-обвязки", str(cm.exception))
        self.assertEqual(kernel.removed, [])
        self.assertEqual(kernel.created, [])

    def test_snapshot_in_raw_export_format_survives_payload_strip(self):
        # Гард --base не ломается стрипом payload'а: снапшот — сырой export
        # (с обвязкой), payload — почищенный контент; сверка идёт по снапшоту
        # и свежему export, оба в экспорт-форме.
        kernel = FakeKernel(docs=[DOC], content=BASE_CONTENT)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / "снапшот.md"
            base.write_text(BASE_CONTENT)
            md = Path(tmp) / "док.md"
            md.write_text("Переписанное тело без обвязки.\n")
            with patch.object(wiki_ops.time, "sleep", lambda s: None):
                run_cmd(wiki_ops.cmd_replace,
                        SimpleNamespace(docId=DOC_ID, file=str(md),
                                        base=str(base), force=False),
                        kernel)
        self.assertEqual(kernel.removed, [DOC_ID])
        self.assertEqual(len(kernel.created), 1)
        self.assertEqual(kernel.created[0]["markdown"], "Переписанное тело без обвязки.\n")

    def test_replace_twice_by_printed_ids_keeps_chain_alive(self):
        # «replace дважды подряд по id из вывода»: id из первого вывода — живая
        # цель второго; export по цепочке не даёт пустоты.
        kernel = FakeKernel(docs=[DOC], content=BASE_CONTENT)
        seen_ids = [DOC_ID]
        for step in range(2):
            new_id = f"20261108000000-chain{step:02d}"
            kernel.create_returns = new_id
            kernel.docs[new_id] = dict(DOC, id=new_id)
            with tempfile.TemporaryDirectory() as tmp:
                base = Path(tmp) / "снапшот.md"
                base.write_text(BASE_CONTENT)
                md = Path(tmp) / "док.md"
                md.write_text(f"Тело шага {step}.\n")
                with patch.object(wiki_ops.time, "sleep", lambda s: None):
                    out, _ = run_cmd(wiki_ops.cmd_replace,
                                     SimpleNamespace(docId=seen_ids[-1], file=str(md),
                                                     base=str(base), force=False),
                                     kernel)
            self.assertIn(f"новый docId: {new_id}", out)
            seen_ids.append(new_id)
        self.assertEqual(kernel.removed, seen_ids[:-1])
        self.assertEqual([c["markdown"] for c in kernel.created],
                         ["Тело шага 0.\n", "Тело шага 1.\n"])


if __name__ == "__main__":
    unittest.main()
