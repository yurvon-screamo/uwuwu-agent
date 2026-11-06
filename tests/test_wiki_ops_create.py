"""create резолвит ноутбук по первому сегменту --parent; post() бейлится по ADR-027.

Контракты:
  1) нормализация --parent: ведущий слэш добавляется, хвостовые и повторные
     снимаются/сворачиваются («Experience/tools» из инцидента 04.11 — валидная форма);
  2) первый сегмент — имя ноутбука (приоритет 1, hpath = остаток пути) или корневой
     док ровно одного ноутбука (приоритет 2, hpath = путь как есть); неоднозначный
     корень и неизвестный префикс — RuntimeError с вариантами/списком ноутбуков;
  3) пустой --parent — легаси-дефолт Projects;
  4) «metadata is not ready» в post(): ровно METADATA_BAIL_ATTEMPTS попыток с паузами
     +2s/+5s → различимый bail с хинтом; транспорт — прежний бюджет ретраев;
     перманентный code≠0 — KernelRefused fail-fast (merge PR #80);
  5) сырой «/» в тайтле санитизируется в ∕ (U+2215) — ровно ОДИН вызов
     createDocWithMd; сырой «/» становится разделителем пути, и ядро молча
     создаёт цепочку вложенных доков-оболочек (инцидент 06.11, /devops).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_create.py
"""
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

EXP, PROJ, DAILY = "nb-experience", "nb-projects", "nb-daily"
# «Experience» в корнях Projects — живое зеркало-хвост миграции (приоритет 1 должен
# вести в ноутбук Experience, а не в зеркало); «patterns» — живая неоднозначность.
FIXTURE_ROOTS = {
    "Experience": ["affine", "cli", "testing", "patterns"],
    "Projects": ["uwuwu", "patterns", "workflow", "Experience"],
    "Daily": ["2026-11-01", "2026-11-02"],
}
METADATA_ENVELOPE = {"code": -1, "msg": "document metadata is not ready"}


class FakeKernel:
    """Двойник kernel API: lsNotebooks + listDocsByPath + createDocWithMd (успех)."""

    def __init__(self, roots: dict[str, list[str]] | None = None):
        self.roots = roots or FIXTURE_ROOTS
        self.ids = {"Experience": EXP, "Projects": PROJ, "Daily": DAILY}
        self.calls: list[tuple[str, dict]] = []

    def post(self, path: str, body: dict, attempts: int = 6):
        self.calls.append((path, body))
        if path == "/api/notebook/lsNotebooks":
            notebooks = [{"id": nid, "name": name, "closed": False}
                         for name, nid in self.ids.items()]
            return {"notebooks": notebooks}
        if path == "/api/filetree/listDocsByPath":
            name = next(n for n, i in self.ids.items() if i == body["notebook"])
            return {"files": [{"name": r} for r in self.roots.get(name, [])]}
        if path == "/api/filetree/createDocWithMd":
            return None
        raise AssertionError(f"неожиданный путь: {path}")

    def create_calls(self) -> list[dict]:
        return [body for p, body in self.calls if p == "/api/filetree/createDocWithMd"]


def run_create(kernel: FakeKernel, parent: str, title: str = "Probe") -> str:
    md_file = tempfile.NamedTemporaryFile("w", suffix=".md", delete=False)
    try:
        md_file.write("# probe\n")
        md_file.close()
        original_post = wiki_ops.post
        wiki_ops.post = kernel.post
        buffer = io.StringIO()
        try:
            args = SimpleNamespace(title=title, file=md_file.name, parent=parent)
            with redirect_stdout(buffer):
                wiki_ops.cmd_create(args)
        finally:
            wiki_ops.post = original_post
    finally:
        os.unlink(md_file.name)
    return buffer.getvalue()


class ParentResolution(unittest.TestCase):
    """create пишет в ноутбук, выбранный по корневому контейнеру из --parent."""

    def test_project_root_folder_targets_projects(self):
        kernel = FakeKernel()
        run_create(kernel, "/uwuwu/ADR")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], PROJ)
        self.assertEqual(call["path"], "/uwuwu/ADR/Probe")

    def test_experience_theme_targets_experience(self):
        kernel = FakeKernel()
        run_create(kernel, "/affine")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], EXP)
        self.assertEqual(call["path"], "/affine/Probe")

    def test_daily_date_root_targets_daily(self):
        kernel = FakeKernel()
        run_create(kernel, "/2026-11-01")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], DAILY)
        self.assertEqual(call["path"], "/2026-11-01/Probe")

    def test_trailing_slash_is_stripped(self):
        kernel = FakeKernel()
        run_create(kernel, "/affine/")
        self.assertEqual(kernel.create_calls()[0]["path"], "/affine/Probe")

    def test_output_prints_notebook_name(self):
        output = run_create(FakeKernel(), "/affine")
        self.assertIn("[Experience]", output)


class NotebookNameMode(unittest.TestCase):
    """Приоритет 1: первый сегмент — имя ноутбука, hpath = остаток пути."""

    def test_full_workspace_path_targets_projects(self):
        kernel = FakeKernel()
        run_create(kernel, "/Projects/uwuwu/ADR")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], PROJ)
        self.assertEqual(call["path"], "/uwuwu/ADR/Probe")

    def test_experience_prefix_wins_over_projects_mirror(self):
        kernel = FakeKernel()
        run_create(kernel, "/Experience/cli")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], EXP)
        self.assertEqual(call["path"], "/cli/Probe")

    def test_bare_notebook_name_targets_notebook_root(self):
        kernel = FakeKernel()
        run_create(kernel, "/Projects")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], PROJ)
        self.assertEqual(call["path"], "/Probe")


class ParentNormalization(unittest.TestCase):
    """Дикие формы --parent из живых инцидентов нормализуются."""

    def test_missing_leading_slash(self):
        kernel = FakeKernel()
        run_create(kernel, "Experience/tools")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], EXP)
        self.assertEqual(call["path"], "/tools/Probe")

    def test_double_slashes_collapse(self):
        kernel = FakeKernel()
        run_create(kernel, "/uwuwu//ADR")
        self.assertEqual(kernel.create_calls()[0]["path"], "/uwuwu/ADR/Probe")


class DefaultsAndErrors(unittest.TestCase):
    """Пустой --parent — легаси-дефолт Projects; ошибки несут варианты."""

    def test_empty_parent_defaults_to_projects(self):
        kernel = FakeKernel()
        run_create(kernel, "")
        call = kernel.create_calls()[0]
        self.assertEqual(call["notebook"], PROJ)
        self.assertEqual(call["path"], "/Probe")

    def test_unknown_prefix_lists_all_notebooks(self):
        kernel = FakeKernel()
        with self.assertRaises(RuntimeError) as cm:
            run_create(kernel, "/nope")
        for name in ("Experience", "Projects", "Daily"):
            self.assertIn(name, str(cm.exception))
        self.assertEqual(kernel.create_calls(), [])

    def test_ambiguous_root_suggests_disambiguation_syntax(self):
        kernel = FakeKernel()
        with self.assertRaises(RuntimeError) as cm:
            run_create(kernel, "/patterns")
        message = str(cm.exception)
        self.assertIn("/Projects/patterns", message)
        self.assertIn("/Experience/patterns", message)


class TitleSanitization(unittest.TestCase):
    """Сырой «/» в тайтле не долетает до ядра: ∕-санитайз до build hpath."""

    def test_inner_slashes_become_division_slashes_single_doc(self):
        kernel = FakeKernel()
        run_create(kernel, "/affine", title="A/B/C")
        calls = kernel.create_calls()
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["path"], "/affine/A∕B∕C")

    def test_division_slash_title_passes_through_untouched(self):
        kernel = FakeKernel()
        run_create(kernel, "/affine", title="уже ∕ санитизирован")
        self.assertEqual(kernel.create_calls()[0]["path"], "/affine/уже ∕ санитизирован")

    def test_slash_only_title_rejected_before_kernel(self):
        kernel = FakeKernel()
        with self.assertRaises(RuntimeError) as cm:
            run_create(kernel, "/affine", title="///")
        self.assertEqual(kernel.create_calls(), [])
        self.assertIn("пустой", str(cm.exception))


class FakeResponse:
    def __init__(self, payload: bytes):
        self._payload = payload

    def read(self) -> bytes:
        return self._payload

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeTransport:
    """urlopen-двойник: конверты по очереди (dict) или исключения (Exception);

    список короче числа вызовов — повторяется последний элемент.
    """

    def __init__(self, envelopes):
        self.envelopes = envelopes
        self.requests: list[dict] = []

    def __call__(self, req, timeout=60):
        index = min(len(self.requests), len(self.envelopes) - 1)
        self.requests.append(json.loads(req.data.decode()))
        envelope = self.envelopes[index]
        if isinstance(envelope, Exception):
            raise envelope
        return FakeResponse(json.dumps(envelope).encode())


class MetadataBailDiscipline(unittest.TestCase):
    """post(): metadata-not-ready — ≤3 попытки → различимый bail; транспорт — полный
    бюджет ретраев; перманентный code≠0 — KernelRefused fail-fast (merge PR #80)."""

    def setUp(self):
        self._url, self._token = wiki_ops.URL, wiki_ops.TOKEN
        wiki_ops.URL = "http://kernel.invalid"
        wiki_ops.TOKEN = "test-token"  # nosec B105 — фиктивный токен для urlopen-двойника, не секрет
        self.addCleanup(self._restore_conf)
        self.sleeps: list[float] = []
        sleep_patch = patch.object(wiki_ops.time, "sleep", self.sleeps.append)
        sleep_patch.start()
        self.addCleanup(sleep_patch.stop)

    def _restore_conf(self):
        wiki_ops.URL, wiki_ops.TOKEN = self._url, self._token

    def _run_post(self, envelopes, path: str) -> tuple[FakeTransport, RuntimeError]:
        transport = FakeTransport(envelopes)
        with patch.object(wiki_ops.urllib.request, "urlopen", transport):
            with self.assertRaises(RuntimeError) as cm:
                wiki_ops.post(path, {})
        return transport, cm.exception

    def test_metadata_bails_after_three_attempts_with_hint(self):
        transport, error = self._run_post([METADATA_ENVELOPE], "/api/filetree/createDocWithMd")
        self.assertEqual(len(transport.requests), wiki_ops.METADATA_BAIL_ATTEMPTS)
        self.assertEqual(self.sleeps, [2, 5])
        message = str(error)
        self.assertIn("metadata is not ready", message)
        self.assertIn("uwuwu-cli doctor", message)

    def test_create_hint_names_parent_and_title_causes(self):
        _, error = self._run_post([METADATA_ENVELOPE], "/api/filetree/createDocWithMd")
        message = str(error)
        self.assertIn("родитель", message)
        self.assertIn("тайтл", message)

    def test_append_hint_is_neutral(self):
        _, error = self._run_post([METADATA_ENVELOPE], "/api/block/appendBlock")
        message = str(error)
        self.assertIn("uwuwu-cli doctor", message)
        self.assertNotIn("тайтл", message)
        self.assertNotIn("родитель", message)

    def test_other_error_code_is_permanent_refusal_fail_fast(self):
        # Объединённая семантика (merge PR #80): перманентный code≠0 — KernelRefused
        # немедленно, без ретраев. Намерение исходного пина («прочее ≠ metadata-bail»)
        # сохранено: класс отличен, bail-хинта в сообщении нет.
        transport, error = self._run_post([{"code": -1, "msg": "boom"}], "/api/query/sql")
        self.assertEqual(len(transport.requests), 1)
        self.assertIsInstance(error, wiki_ops.KernelRefused)
        self.assertNotIn("metadata-bail", str(error))

    def test_transport_error_keeps_full_retries(self):
        transport, error = self._run_post([OSError("connection reset")], "/api/query/sql")
        self.assertEqual(len(transport.requests), 6)
        self.assertIn("connection reset", str(error))


if __name__ == "__main__":
    unittest.main()
