"""Стрип экспорт-обвязки на входе doc-level write-path (create/replace/append).

Контракты (_strip_export_wrapper — порт cli strip_export_wrapper
cli/src/markdown.rs; канон — опыт «SiYuan kernel write-path: SQL-индекс
отстаёт от мутаций, ZWJ-заглушка, frontmatter в экспорте», инцидент 08.11 —
литеральные дубли обвязки в доках):
  1) сырой export (frontmatter + ведущий title-H1 + тело) → чистое тело,
     notes перечисляет снятое;
  2) payload без обвязки → как есть, notes пуст;
  3) ведущий H1 снимается только по кандидату (title_candidates ∪ fm-title):
     чужая ведущая H1, H1 в середине дока и H1 с другим регистром не трогаются;
  4) обе формы тайтла create (сырой «A/B» и ∕-форма) матчатся;
  5) незакрытый frontmatter — не стрипается (паритет cli);
  6) CRLF-вход — известный край (LF-контракт, паритет cli): fm-префикс
     не матчит, обвязка проходит насквозь;
  7) reimport чужого экспорта: fm + ведущий H1 источника снимаются
     (fm-title — кандидат: H1 из того же export'а — артефакт, не контент);
  8) payload только из frontmatter стрипается в пустоту — вызывающий
     (cmd_*) отказывает.

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_strip.py
"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops


def strip(payload: str, candidates: tuple[str, ...] = ()) -> tuple[str, list[str]]:
    return wiki_ops._strip_export_wrapper(payload, candidates)


RAW_EXPORT = ("---\ntitle: ADR-063: Проба\ndate: 2026-11-08T01:00:00+08:00\n"
              "lastmod: 2026-11-08T02:00:00+08:00\n---\n\n# ADR-063: Проба\n\nТело.\n")


class RawExportStripped(unittest.TestCase):
    """Инцидент 08.11: сырой export в payload — обвязка не доходит до дока."""

    def test_full_export_is_stripped_to_body(self):
        clean, notes = strip(RAW_EXPORT, ("ADR-063: Проба",))
        self.assertEqual(clean, "Тело.\n")
        self.assertEqual(notes, ["frontmatter", "title-H1"])

    def test_export_without_external_candidates_strips_by_fm_title(self):
        # append-сценарий: кандидатов снаружи нет, fm-title payload'а — кандидат.
        clean, notes = strip(RAW_EXPORT)
        self.assertEqual(clean, "Тело.\n")
        self.assertEqual(notes, ["frontmatter", "title-H1"])

    def test_frontmatter_only_payload_empties(self):
        clean, notes = strip("---\ntitle: x\n---\n")
        self.assertEqual(clean, "")
        self.assertEqual(notes, ["frontmatter"])

    def test_trailing_whitespace_on_title_line_still_matches(self):
        clean, notes = strip("---\ntitle: x\n---\n\n# x  \nтело\n")
        self.assertEqual(clean, "тело\n")
        self.assertEqual(notes, ["frontmatter", "title-H1"])


class UntouchedPayloads(unittest.TestCase):
    """Без обвязки — байт-как-есть; чужой/не-ведущий H1 — не трогается."""

    def test_payload_without_wrapper_untouched(self):
        payload = "## Раздел\n\nтекст\n"
        clean, notes = strip(payload, ("Раздел",))
        self.assertEqual(clean, payload)
        self.assertEqual(notes, [])

    def test_foreign_leading_h1_untouched(self):
        payload = "# Другой заголовок\nтекст\n"
        clean, notes = strip(payload, ("ADR-063: Проба",))
        self.assertEqual(clean, payload)
        self.assertEqual(notes, [])

    def test_mid_document_title_h1_untouched(self):
        # scope — ведущая строка: повторная титульная строка в середине —
        # контракт cli strip_title_h1, у wiki_ops его нет.
        clean, notes = strip("# X\n\nтело\n\n# X\n", ("X",))
        self.assertEqual(clean, "тело\n\n# X\n")
        self.assertEqual(notes, ["title-H1"])

    def test_title_case_mismatch_is_foreign_h1(self):
        clean, notes = strip("# probe\nтело\n", ("Probe",))
        self.assertEqual(clean, "# probe\nтело\n")
        self.assertEqual(notes, [])

    def test_fm_without_title_keeps_leading_h1(self):
        # fm-стрип безусловен (frontmatter — всегда экспортный мусор), но без
        # title в fm и внешних кандидатов ведущий H1 не матчится — остаётся.
        payload = "---\ndate: 2026-11-08\n---\n\n# Кто-то\nтело\n"
        clean, notes = strip(payload)
        self.assertEqual(clean, "# Кто-то\nтело\n")
        self.assertEqual(notes, ["frontmatter"])

    def test_unclosed_frontmatter_untouched(self):
        payload = "---\ntitle: x\nтело без закрытия\n"
        clean, notes = strip(payload)
        self.assertEqual(clean, payload)
        self.assertEqual(notes, [])


class CreateCandidateForms(unittest.TestCase):
    """create передаёт обе формы тайтла: сырую и path_safe (∕-форма)."""

    def test_raw_slash_form_matches(self):
        clean, notes = strip("# A/B: тест\nтело\n", ("A/B: тест", "A∕B: тест"))
        self.assertEqual(clean, "тело\n")
        self.assertEqual(notes, ["title-H1"])

    def test_division_slash_form_matches(self):
        clean, notes = strip("# A∕B: тест\nтело\n", ("A/B: тест", "A∕B: тест"))
        self.assertEqual(clean, "тело\n")
        self.assertEqual(notes, ["title-H1"])


class KnownEdges(unittest.TestCase):
    """Документированные края: CRLF-вход, hr-коллизия."""

    def test_crlf_payload_is_documented_miss(self):
        # LF-контракт (паритет cli): CRLF-обвязка проходит насквозь —
        # осознанный край, не тихий стрип с порчей.
        payload = "---\r\ntitle: x\r\n---\r\n\r\n# x\r\nтело\r\n"
        clean, notes = strip(payload)
        self.assertEqual(clean, payload)
        self.assertEqual(notes, [])

    def test_thematic_break_pair_strips_as_frontmatter(self):
        # hr-коллизия: «--- … ---» в начале безусловно читается как frontmatter
        # (докстринг _strip_export_wrapper); примечание вызывающего делает
        # трансформацию видимой.
        clean, notes = strip("---\nпроза между брейками\n---\nтело\n")
        self.assertEqual(clean, "тело\n")
        self.assertEqual(notes, ["frontmatter"])

    def test_fm_title_extraction_tolerates_spaces(self):
        clean, _ = strip("---\ntitle:    Док с пробелами\n---\n# Док с пробелами\nт\n")
        self.assertEqual(clean, "т\n")


if __name__ == "__main__":
    unittest.main()
