"""orgls обязан показывать полное дерево воркспейса (SiYuan).

Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки
(подтверждено на живом ядре 04.11: без limit → 64; limit 65 → 65;
limit 100000 → все строки). Контракты:
  1) каждый stmt orgls несёт явный limit;
  2) клиентская сторона не режет выдачу сервера;
  3) живой инвариант — вывод покрывает все корневые имена listDocsByPath
     и строки-документы каждой секции == count(*) блоков ноутбука.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_orgls.py
"""
import io
import re
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

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

    def __init__(self, notebook_count: int = 1, kernel_cuts_unlimited: bool = True):
        self.calls: list[tuple[str, dict]] = []
        self.notebook_count = notebook_count
        self.kernel_cuts_unlimited = kernel_cuts_unlimited

    def post(self, path: str, body: dict, attempts: int = 6):
        self.calls.append((path, body))
        if path == "/api/notebook/lsNotebooks":
            notebooks = [{"id": f"nb{i}", "name": f"NB{i}", "closed": False}
                         for i in range(self.notebook_count)]
            return {"notebooks": notebooks}
        if path == "/api/query/sql":
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
    original_post = wiki_ops.post
    wiki_ops.post = post_impl
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer):
            wiki_ops.cmd_orgls(type("Args", (), {})())
    finally:
        wiki_ops.post = original_post
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


if __name__ == "__main__":
    unittest.main()
