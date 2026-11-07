"""orgls обязан показывать полное дерево воркспейса (SiYuan).

Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки
(подтверждено на живом ядре 04.11: без limit → 64; limit 65 → 65;
limit 100000 → все строки). Контракты:
  1) каждый stmt orgls несёт явный limit;
  2) клиентская сторона не режет выдачу сервера;
  3) живой инвариант — вывод покрывает все корневые имена listDocsByPath
     и строки-документы каждой секции == count(*) блоков ноутбука.

Контракты сбоя (задача 20261104072714 — «сбой маскируется под "не найден"»):
  4) сбой SQL по ноутбуку посреди обхода → ошибка с именем ноутбука и
     последней причиной, exit 1, stdout ПУСТ (атомарность: частичное
     дерево нельзя прочитать как полное);
  5) «не найден» в тексте ошибки нет — сбой ≠ not-found;
  6) аномальный конверт lsNotebooks (data=None, dict без "notebooks") →
     громкая ошибка, не «пустой воркспейс» с exit 0; пустой notebooks —
     легитимный not-found (round-2 ревью PR #111, High).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_orgls.py
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
from tests.wiki_ops_testkit import kernel_post

KERNEL_DEFAULT_LIMIT = 64
FIXTURE_DOCS = 250
EXPERIENCE_MIN_DOCS = 30
PROJECTS_MIN_DOCS = 7


class FakeKernel:
    """Двойник kernel API: lsNotebooks + query/sql с режимом дефолт-лимита.

    kernel_cuts_unlimited=True эмулирует живое ядро: stmt без LIMIT →
    первые KERNEL_DEFAULT_LIMIT строк; False — сервер честно отдаёт всё
    (для проверки клиентского капа).
    """

    def __init__(self, notebook_count: int = 1, kernel_cuts_unlimited: bool = True,
                 fail_sql_for: str | None = None, ls_payload="__normal__",
                 closed_count: int = 0):
        self.calls: list[tuple[str, dict]] = []
        self.notebook_count = notebook_count
        self.kernel_cuts_unlimited = kernel_cuts_unlimited
        self.fail_sql_for = fail_sql_for
        self.ls_payload = ls_payload
        self.closed_count = closed_count

    def post(self, path: str, body: dict, attempts: int = 6):
        self.calls.append((path, body))
        if path == "/api/notebook/lsNotebooks":
            if self.ls_payload != "__normal__":
                return self.ls_payload
            notebooks = [{"id": f"nb{i}", "name": f"NB{i}",
                          "closed": i >= self.notebook_count - self.closed_count}
                         for i in range(self.notebook_count)]
            return {"notebooks": notebooks}
        if path == "/api/query/sql":
            if self.fail_sql_for is not None and self.fail_sql_for in body["stmt"]:
                raise RuntimeError(f"POST {path}: code=500 msg=boom")
            limit = self._stmt_limit(body["stmt"])
            if limit is None and self.kernel_cuts_unlimited:
                limit = KERNEL_DEFAULT_LIMIT
            if limit is None:
                limit = FIXTURE_DOCS
            return [{"hpath": f"/doc-{i:04d}"} for i in range(min(limit, FIXTURE_DOCS))]
        raise AssertionError(f"неожиданный путь: {path}")

    @staticmethod
    def _stmt_limit(stmt: str) -> int | None:
        match = re.search(r"limit\s+(\d+)", stmt, re.IGNORECASE)
        return int(match.group(1)) if match else None

    def sql_stmts(self) -> list[str]:
        return [body["stmt"] for path, body in self.calls if path == "/api/query/sql"]


def run_orgls(post_impl) -> str:
    buffer = io.StringIO()
    with kernel_post(post_impl), redirect_stdout(buffer):
        wiki_ops.cmd_orgls(type("Args", (), {})())
    return buffer.getvalue()


def parse_sections(output: str) -> dict[str, list[str]]:
    """Секции «[Ноутбук]» → список печатных имён доков (без заголовка)."""
    sections: dict[str, list[str]] = {}
    current: list[str] | None = None
    for line in output.splitlines():
        if line.startswith("[") and line.endswith("]"):
            current = sections.setdefault(line[1:-1], [])
            continue
        if current is not None and line.strip():
            current.append(line.strip())
    return sections


class OrglsFetchesWholeTree(unittest.TestCase):
    """Контракт против молчаливого дефолт-лимита ядра."""

    def test_kernel_default_cap_shows_all_docs(self):
        kernel = FakeKernel(kernel_cuts_unlimited=True)
        output = run_orgls(kernel.post)
        self.assertEqual(len(re.findall(r"doc-\d{4}", output)), FIXTURE_DOCS)

    def test_generous_server_output_not_cut_client_side(self):
        kernel = FakeKernel(kernel_cuts_unlimited=False)
        output = run_orgls(kernel.post)
        self.assertEqual(len(re.findall(r"doc-\d{4}", output)), FIXTURE_DOCS)

    def test_every_sql_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(kernel_cuts_unlimited=False)
        run_orgls(kernel.post)
        stmts = kernel.sql_stmts()
        self.assertTrue(stmts, "orgls не выполнил ни одного sql-запроса")
        for stmt in stmts:
            self.assertIsNotNone(re.search(r"limit\s+\d+", stmt, re.IGNORECASE),
                                 f"stmt без явного LIMIT: {stmt}")


class LiveWorkspaceInvariant(unittest.TestCase):
    """Живое ядро: полнота дерева против listDocsByPath и count(*)."""

    def setUp(self):
        try:
            wiki_ops.notebooks()
        except Exception:
            self.skipTest("SiYuan kernel недоступен")

    def test_output_covers_roots_and_matches_block_counts(self):
        sections = parse_sections(run_orgls(wiki_ops.post))
        for nb in wiki_ops.notebooks():
            with self.subTest(notebook=nb["name"]):
                doc_names = sections.get(nb["name"], [])
                roots = wiki_ops.post("/api/filetree/listDocsByPath",
                                      {"notebook": nb["id"], "path": "/"})
                for root in (roots.get("files") or []):
                    self.assertIn(root["name"], doc_names,
                                  f"корень «{root['name']}» отсутствует в orgls")
                total = wiki_ops.post(
                    "/api/query/sql",
                    {"stmt": f"select count(*) c from blocks where type='d' and box='{nb['id']}' limit 1"})  # nosec B608 — id ноутбука из kernel API, не пользовательский ввод
                self.assertEqual(len(doc_names), total[0]["c"],
                                 f"строки-документы ({len(doc_names)}) != count(*) ({total[0]['c']})")
        self.assertGreaterEqual(len(sections.get("Experience", [])), EXPERIENCE_MIN_DOCS)
        self.assertGreaterEqual(len(sections.get("Projects", [])), PROJECTS_MIN_DOCS)


class OrglsMainHarness(unittest.TestCase):
    """Общий харнесс main-уровня: отказ обязан завершаться exit 1.
    Возвращает (stdout, stderr); ассерты кода — здесь, контента — в тестах."""

    def run_orgls_via_main(self, kernel) -> tuple[str, str]:
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", ["wiki_ops.py", "orgls"]), \
                patch.object(wiki_ops, "post", kernel.post), \
                redirect_stdout(out), redirect_stderr(err):
            with self.assertRaises(SystemExit) as cm:
                wiki_ops.main()
        self.assertEqual(cm.exception.code, 1, "сбой обязан завершаться exit 1")
        return out.getvalue(), err.getvalue()


class NotebooksEnvelopeIsClassified(OrglsMainHarness):
    """Аномалия lsNotebooks — громкий RuntimeError; пустой notebooks легитимен."""

    def test_ls_payload_none_raises_not_attribute_error(self):
        kernel = FakeKernel(ls_payload=None)
        out, err = self.run_orgls_via_main(kernel)
        self.assertIn("ERROR:", err)
        self.assertIn("lsNotebooks", err)
        self.assertEqual(out, "")
        self.assertNotIn("не найден", err)
        self.assertNotIn("не найдено", err)

    def test_ls_dict_without_notebooks_raises_not_silent_empty(self):
        kernel = FakeKernel(ls_payload={"code": 0})
        out, err = self.run_orgls_via_main(kernel)
        self.assertIn("ERROR:", err)
        self.assertIn("lsNotebooks", err)
        self.assertEqual(out, "", "пустой stdout обязателен: «пустой воркспейс» — это маска сбоя")

    def test_empty_notebooks_is_legitimate_not_found(self):
        kernel = FakeKernel(notebook_count=0)
        with kernel_post(kernel):
            self.assertEqual(wiki_ops.notebooks(), [])

    def test_closed_notebooks_filtered_out(self):
        kernel = FakeKernel(notebook_count=3, closed_count=1)
        with kernel_post(kernel):
            names = [nb["name"] for nb in wiki_ops.notebooks()]
        self.assertEqual(names, ["NB0", "NB1"],
                         "закрытый NB2 обязан отфильтровываться (иначе тест пустой)")


class MidTreeFailureIsLoudAndAtomic(OrglsMainHarness):
    """Сбой SQL по ноутбуку посреди обхода ≠ «не найдено»: причина + пустой stdout."""

    def test_mid_tree_sql_failure_names_notebook_and_keeps_stdout_empty(self):
        kernel = FakeKernel(notebook_count=2, fail_sql_for="nb1")
        out, err = self.run_orgls_via_main(kernel)
        self.assertIn("ERROR:", err)
        self.assertIn("orgls", err)
        self.assertIn("NB1", err, "имя упавшего ноутбука не дошло до ошибки")
        self.assertIn("code=500", err, "последняя причина не дошла до ошибки")
        self.assertNotIn("не найден", err)
        self.assertNotIn("не найдено", err)
        self.assertEqual(out, "", "частичное дерево в stdout читается как полное")


if __name__ == "__main__":
    unittest.main()
