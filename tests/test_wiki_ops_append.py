"""append обязан быть идемпотентным при ретраях: guard-луп со сверкой хвоста export.

Контракты (живые пробы на ядре 06.11, задача 20261104072049-9gl0029):
  1) реплей инцидента append ×4: appendBlock применил и «уронил» ответ →
     следующая попытка видит контент в хвосте → ровно один appendBlock,
     репорт «already applied», rc 0;
  2) слепых ретраев нет: шаги лупа (export, appendBlock) — attempts=1,
     единственная точка ретраев — сам guard-луп (N=6, worst case ≤7 мин);
  3) нормализация хвоста сливает только рендер-неразличимое: whitespace-разбеги
     и пустые строки вне fence (рендер стирает; ядро разыхивает списки) и
     whitespace у pipe в таблицах (ядро стирает при roundtrip — проба 1);
     ведущий whitespace вне fence — дословно (контрпример «- x» vs «  - x»:
     тихая потеря записи запрещена); код внутри fence — дословно;
     переиндентация 4→2 индет-блока — задокументированный промах в безопасную
     сторону (видимый дубль);
  4) перманентный отказ ядра (KernelRefused) — немедленный raise без ретраев;
     export недоступен все попытки → fail-closed: ноль appendBlock, громкая
     ошибка;
  5) tail-семантика: идентичное в середине дока — не no-op; --force обходит
     guard;
  6) docId валидируется до любых вызовов API.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_append.py
"""
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.error
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

DOC = "20261106000000-abcdefg"


class FakeKernel:
    """Двойник kernel API: export-контенты, appendBlock (применяет + опционально роняет ответ).

    apply_loses_response — режим инцидента: блок применён (export это увидит),
    ответ клиенту потерян (transport-таймаут). append_code — бизнес-отказ ядра.
    """

    def __init__(self, contents=None, apply_loses_response=False, append_code=None):
        self.contents = dict(contents or {})
        self.apply_loses_response = apply_loses_response
        self.append_code = append_code
        self.appends = []
        self.export_calls = 0

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/export/exportMdContent":
            self.export_calls += 1
            return {"content": self.contents.get(body["id"], "")}
        if path == "/api/block/appendBlock":
            self.appends.append(body["data"])
            if self.append_code is not None:
                raise wiki_ops.KernelRefused(
                    f"POST {path}: code={self.append_code['code']} msg={self.append_code['msg']}")
            if self.apply_loses_response:
                parent = body["parentID"]
                self.contents[parent] = self.contents.get(parent, "") + body["data"]
                raise RuntimeError("POST /api/block/appendBlock: <urlopen> timed out")
            return None
        raise AssertionError(f"неожиданный путь: {path}")


def run_append(kernel: FakeKernel, doc_id: str, md: str, force: bool = False) -> str:
    with tempfile.TemporaryDirectory() as tmp:
        md_file = Path(tmp) / "append.md"
        md_file.write_text(md, encoding="utf-8")
        args = SimpleNamespace(docId=doc_id, file=str(md_file), force=force)
        original_post = wiki_ops.post
        wiki_ops.post = kernel.post
        try:
            buffer = io.StringIO()
            with redirect_stdout(buffer), patch.object(wiki_ops.time, "sleep", lambda s: None):
                wiki_ops.cmd_append(args)
        finally:
            wiki_ops.post = original_post
    return buffer.getvalue()


class IncidentReplay(unittest.TestCase):
    """Инцидент append ×4: применённый при потерянном ответе блок не дублируется."""

    def test_lost_response_applies_once_and_reports_already_applied(self):
        kernel = FakeKernel(contents={DOC: "Базовый параграф.\n"}, apply_loses_response=True)
        output = run_append(kernel, DOC, "## Секция\n\nтекст секции\n", force=False)
        self.assertEqual(len(kernel.appends), 1, f"ожидался ровно один appendBlock: {kernel.appends}")
        self.assertEqual(kernel.contents[DOC].count("## Секция"), 1, "одна копия контента в доке")
        self.assertIn("already applied", output)
        self.assertNotIn("дописано", output)


class FreshAndDifferent(unittest.TestCase):
    """Новый контент аппендится; guard не даёт ложного no-op на разный контент."""

    def test_fresh_content_is_appended(self):
        kernel = FakeKernel(contents={DOC: "Базовый параграф.\n"})
        output = run_append(kernel, DOC, "## Секция\n\nтекст\n")
        self.assertEqual(kernel.appends, ["## Секция\n\nтекст\n"])
        self.assertIn("дописано", output)

    def test_different_tail_content_is_appended(self):
        kernel = FakeKernel(contents={DOC: "## Другое\n\nчужой текст\n"})
        run_append(kernel, DOC, "## Секция\n\nтекст\n")
        self.assertEqual(len(kernel.appends), 1)


class NormalizationContract(unittest.TestCase):
    """Спека нормализации по живым пробам 1/2: слипается только рендер-неразличимое.

    Экспортные формы предварительно запечены (ядро «применило и переформатировало
    до теста»); guard должен узнавать в них исходный аппенд. Промах guard'а
    виден по факту повторного appendBlock.
    """

    TABLE_ORIGINAL = "| col | знач |\n|-----|------|\n|  1  |  два |\n"
    TABLE_EXPORTED = "|col|знач|\n| -----| ----------|\n|1|два|\n"

    LIST_ORIGINAL = "- верхний\n  - вложенный\n    - глубже\n- снова верхний\n"
    LIST_EXPORTED = "- верхний\n\n  - вложенный\n    - глубже\n\n- снова верхний\n"

    def test_table_whitespace_mangling_detected_as_applied(self):
        kernel = FakeKernel(contents={DOC: "base\n\n" + self.TABLE_EXPORTED})
        output = run_append(kernel, DOC, self.TABLE_ORIGINAL)
        self.assertEqual(kernel.appends, [], "таблица после манглинга ядра должна узнаваться")
        self.assertIn("already applied", output)

    def test_loose_list_blank_lines_detected_as_applied(self):
        kernel = FakeKernel(contents={DOC: "base\n\n" + self.LIST_EXPORTED})
        run_append(kernel, DOC, self.LIST_ORIGINAL)
        self.assertEqual(kernel.appends, [], "вставленные ядром пустые строки не ломают узнавание")

    def test_top_level_vs_indented_list_item_not_merged(self):
        kernel = FakeKernel(contents={DOC: "base\n\n  - x\n"})
        run_append(kernel, DOC, "- x\n")
        self.assertEqual(len(kernel.appends), 1,
                         "ведущий отступ дословен: подсписок не равен верхнему уровню")

    def test_reindented_block_is_documented_miss(self):
        # Проба 2: индет-блок 4→2 — осознанный промах в безопасную сторону
        # (видимый дубль), тихой потери записи быть не должно.
        kernel = FakeKernel(contents={DOC: "base\n\n  строка индет-блока\n"})
        run_append(kernel, DOC, "    строка индет-блока\n")
        self.assertEqual(len(kernel.appends), 1)

    def test_fenced_code_internal_spaces_not_merged(self):
        exported = "base\n\n```\nfenced:\n    x =  1\n```\n"
        kernel = FakeKernel(contents={DOC: exported})
        run_append(kernel, DOC, "```\nfenced:\n    x = 1\n```\n")
        self.assertEqual(len(kernel.appends), 1, "внутренние пробелы кода семантичны")


class TailSemantics(unittest.TestCase):
    """No-op только на хвост: середина дока и --force живут по прежним правилам."""

    def test_identical_in_middle_is_not_tail_noop(self):
        kernel = FakeKernel(contents={DOC: "## Секция\n\nтекст\n\n## Другое\n\nхвост\n"})
        run_append(kernel, DOC, "## Секция\n\nтекст\n")
        self.assertEqual(len(kernel.appends), 1)

    def test_force_bypasses_guard(self):
        kernel = FakeKernel(contents={DOC: "## Секция\n\nтекст\n"})
        output = run_append(kernel, DOC, "## Секция\n\nтекст\n", force=True)
        self.assertEqual(len(kernel.appends), 1)
        self.assertIn("дописано", output)

    def test_force_is_single_shot_without_blind_retries(self):
        # Инцидент-паттерн не возвращается за флагом: --force при висящем ядре
        # («применил и уронил ответ» каждый раз) даёт ровно одну отсылку и
        # громкую ошибку, N копий не получить.
        kernel = FakeKernel(contents={DOC: "base\n"}, apply_loses_response=True)
        with self.assertRaises(RuntimeError):
            run_append(kernel, DOC, "текст\n", force=True)
        self.assertEqual(len(kernel.appends), 1, "force — single-shot, без ретрай-лупа")
        self.assertEqual(kernel.contents[DOC].count("текст"), 1)


class FailureSemantics(unittest.TestCase):
    """KernelRefused — fail fast; export недоступен — fail-closed; бюджет лупа N=6."""

    def test_kernel_refused_fails_fast_without_retries(self):
        kernel = FakeKernel(contents={},
                            append_code={"code": 5, "msg": "parent not found"})
        with self.assertRaises(wiki_ops.KernelRefused):
            run_append(kernel, DOC, "текст\n")
        self.assertEqual(len(kernel.appends), 1, "перманентный отказ не ретраится 6 раз")

    def test_unavailable_export_is_fail_closed(self):
        kernel = FakeKernel(contents={DOC: "base\n"})

        def exploding_export(path, body, attempts=6):
            if path == "/api/export/exportMdContent":
                kernel.export_calls += 1
                raise RuntimeError("export gateway hung")
            raise AssertionError(f"неожиданный путь: {path}")

        kernel.post = exploding_export
        with self.assertRaises(RuntimeError) as cm:
            run_append(kernel, DOC, "текст\n")
        self.assertEqual(kernel.appends, [], "unverifiable state не аппендится")
        self.assertEqual(kernel.export_calls, 6, "export ретраится только лупом")
        self.assertIn("не подтвердился", str(cm.exception))

    def test_transient_append_failure_exhausts_budget(self):
        kernel = FakeKernel(contents={DOC: "base\n"}, apply_loses_response=False)

        def hanging_append(path, body, attempts=6):
            if path == "/api/export/exportMdContent":
                kernel.export_calls += 1
                return {"content": kernel.contents.get(body["id"], "")}
            kernel.appends.append(body["data"])
            raise RuntimeError("timed out")

        kernel.post = hanging_append
        with self.assertRaises(RuntimeError):
            run_append(kernel, DOC, "текст\n")
        self.assertEqual(len(kernel.appends), 6, "единственный ретрай-луп: N=6 попыток")

    def test_export_failure_once_then_recovers(self):
        kernel = FakeKernel(contents={DOC: "base\n"})

        def flaky_export(path, body, attempts=6):
            if path == "/api/export/exportMdContent":
                kernel.export_calls += 1
                if kernel.export_calls == 1:
                    raise RuntimeError("transient export hiccup")
                return {"content": kernel.contents.get(body["id"], "")}
            kernel.appends.append(body["data"])
            return None

        kernel.post = flaky_export
        output = run_append(kernel, DOC, "текст\n")
        self.assertEqual(len(kernel.appends), 1)
        self.assertIn("дописано", output)


class InputValidation(unittest.TestCase):
    """Мусорный docId отсекается до любых вызовов API."""

    def test_malformed_doc_id_rejected_before_any_api(self):
        kernel = FakeKernel(contents={})
        with self.assertRaises(RuntimeError) as cm:
            run_append(kernel, "x' or 1=1--", "текст\n")
        self.assertEqual(kernel.appends, [])
        self.assertEqual(kernel.export_calls, 0)
        self.assertIn("неверный docId", str(cm.exception))


class NormalizedLines(unittest.TestCase):
    """Unit контракта нормализации: ведущий отступ дословен, внутренние разбеги схлопнуты.

    Осознанная граница: дроп пустых строк вне fence рендер-различим для
    tight/loose списков — это слипание вариантов одного текста, нужное для
    детекции ретрая после разыхивания списка ядром. Не «чинить».
    """

    def test_leading_whitespace_preserved_verbatim(self):
        self.assertEqual(wiki_ops._normalized_lines("- x\n  - y\n    - z\n"),
                         ["- x", "  - y", "    - z"])

    def test_internal_runs_collapsed_and_trailing_dropped(self):
        self.assertEqual(wiki_ops._normalized_lines("слово  слово\tслово  \n"),
                         ["слово слово слово"])

    def test_prose_single_space_is_render_meaningful_not_merged(self):
        self.assertNotEqual(wiki_ops._normalized_lines("а б\n"), wiki_ops._normalized_lines("аб\n"))

    def test_prose_single_pipe_goes_prose_rule_not_table_rule(self):
        # Одиночный pipe в прозе — не табличная строка: «a | b» и «a|b»
        # рендер-различны и не слипаются (ревью раунд 2, Low).
        self.assertNotEqual(wiki_ops._normalized_lines("a | b\n"),
                            wiki_ops._normalized_lines("a|b\n"))
        self.assertEqual(wiki_ops._normalized_lines("a |  b\n"), ["a | b"])

    def test_table_pipe_adjacent_whitespace_removed(self):
        # Проба 1: ядро стирает пробелы у pipe при roundtrip — слипаем как ядро.
        self.assertEqual(wiki_ops._normalized_lines("| a | b |\n| ---| ---|\n"),
                         ["|a|b|", "|-|-|"])

    def test_table_separator_dash_padding_is_kernel_erased(self):
        # Живая верификация 06.11: ядро пэддит дефисы separator'а до ширины
        # колонок («|-----|» → «| -----|») — скелет двоеточий сохраняем,
        # пэддинг слипаем как ядро.
        self.assertEqual(wiki_ops._normalized_lines("|:---:|---:|"),
                         wiki_ops._normalized_lines("| :--: | --:|"))
        self.assertEqual(wiki_ops._normalized_lines("|:---:|---:|"), ["|:-:|-:|"])

    def test_thematic_break_is_not_table_separator(self):
        # «---» (hr) без pipe не канонизируется и не мёржится с separator'ом.
        self.assertEqual(wiki_ops._normalized_lines("---\n|---|\n"), ["---", "|-|"])
        self.assertNotEqual(wiki_ops._normalized_lines("---\n"),
                            wiki_ops._normalized_lines("|---|\n"))

    def test_table_cell_internal_runs_preserved(self):
        # Слипаем меньше, чем известно про ядро: внутренние пробелы ячейки
        # ядро не пробилось проверять — лишнее слипание = риск тихой потери.
        self.assertEqual(wiki_ops._normalized_lines("|a  b|c|\n"), ["|a  b|c|"])

    def test_blank_lines_dropped(self):
        self.assertEqual(wiki_ops._normalized_lines("a\n\n\nb\n"), ["a", "b"])

    def test_fence_content_verbatim(self):
        self.assertEqual(wiki_ops._normalized_lines("```\nx =  1\n  y\n```\n"),
                         ["```", "x =  1", "  y", "```"])

    def test_indented_fence_toggles_and_content_verbatim(self):
        self.assertEqual(wiki_ops._normalized_lines("текст\n  ```\n  code  here\n  ```\n"),
                         ["текст", "  ```", "  code  here", "  ```"])

    def test_empty_text_normalizes_to_empty(self):
        self.assertEqual(wiki_ops._normalized_lines("\n \n\t\n"), [])


class _FakeResponse:
    def __init__(self, payload: dict):
        self._payload = json.dumps(payload).encode()

    def read(self) -> bytes:
        return self._payload

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class PostClassification(unittest.TestCase):
    """post() различает перманентный отказ ядра и транзиент (typed, не строки)."""

    def setUp(self):
        wiki_ops.URL = None
        wiki_ops.TOKEN = None
        env = {"UWUWU_SIYUAN_URL": "http://mock", "UWUWU_SIYUAN_TOKEN": "stub-token"}
        patcher = patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        wiki_ops.URL = None
        wiki_ops.TOKEN = None

    def test_non_metadata_kernel_code_raises_kernel_refused_immediately(self):
        calls = []
        envelope = {"code": 5, "msg": "parent not found"}

        def fake_urlopen(req, timeout=60):
            calls.append(req)
            return _FakeResponse(envelope)

        sleeps = []
        with (patch.object(wiki_ops.urllib.request, "urlopen", fake_urlopen),
              patch.object(wiki_ops.time, "sleep", sleeps.append)):
            with self.assertRaises(wiki_ops.KernelRefused):
                wiki_ops.post("/api/block/appendBlock", {"data": "x"})
        self.assertEqual(len(calls), 1, "перманентный code≠0 не ретраится")
        self.assertEqual(sleeps, [], "перед немедленным отказом нет пауз")

    def test_metadata_not_ready_is_retried(self):
        envelopes = [{"code": 1, "msg": "metadata is not ready"}, {"code": 0, "data": "ok"}]

        def fake_urlopen(req, timeout=60):
            return _FakeResponse(envelopes.pop(0))

        with (patch.object(wiki_ops.urllib.request, "urlopen", fake_urlopen),
              patch.object(wiki_ops.time, "sleep", lambda s: None)):
            self.assertEqual(wiki_ops.post("/api/query/sql", {"stmt": "select 1"}), "ok")

    def test_transport_error_retried_then_raises(self):
        def fake_urlopen(req, timeout=60):
            raise urllib.error.URLError("conn refused")

        with (patch.object(wiki_ops.urllib.request, "urlopen", fake_urlopen),
              patch.object(wiki_ops.time, "sleep", lambda s: None)):
            with self.assertRaises(RuntimeError) as cm:
                wiki_ops.post("/api/query/sql", {"stmt": "select 1"}, attempts=3)
        self.assertIn("conn refused", str(cm.exception))

    def test_single_attempt_has_no_inner_sleep(self):
        # attempts=1 — без внутреннего backoff: двойного бэкоффа с guard-лупом
        # нет, «единственная точка ретраев — луп» абсолютна (ревью раунд 2, Low).
        def fake_urlopen(req, timeout=60):
            raise urllib.error.URLError("conn refused")

        sleeps = []
        with (patch.object(wiki_ops.urllib.request, "urlopen", fake_urlopen),
              patch.object(wiki_ops.time, "sleep", sleeps.append)):
            with self.assertRaises(RuntimeError):
                wiki_ops.post("/api/query/sql", {"stmt": "select 1"}, attempts=1)
        self.assertEqual(sleeps, [])


if __name__ == "__main__":
    unittest.main()
