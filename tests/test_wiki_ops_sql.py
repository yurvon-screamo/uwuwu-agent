"""SQL-хелперы не отвечают пустым успехом на аномальный конверт ядра.

Контракт (задача 20261104072714 — «сбой маскируется под "не найден"»):
  1) /api/query/sql обязан вернуть список строк: list → как есть; dict с
     list-полем "data" → unwrap (совместимость); всё остальное (None, dict
     без "data", скаляры) → RuntimeError «аномальный конверт» — голый
     AttributeError/тихий [] маскируют сбой под «строк нет»;
  2) count-запрос (_sql_scalar) на здоровом ядре всегда возвращает ровно
     одну строку: пустой rows → RuntimeError (fail-closed), не тихий 0 —
     0 у delete-guard'ов означает «сноси без --force».

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_sql.py
"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

COUNT_STMT = "select count(*) c from blocks where type='d' limit 1"


def post_returning(payload):
    def post(path, body, attempts=6):
        if path != "/api/query/sql":
            raise AssertionError(f"неожиданный путь: {path}")
        return payload
    return post


def run_sql_rows(payload):
    original_post = wiki_ops.post
    wiki_ops.post = post_returning(payload)
    try:
        return wiki_ops._sql_rows("select 1 limit 1")
    finally:
        wiki_ops.post = original_post


def run_sql_scalar(payload):
    original_post = wiki_ops.post
    wiki_ops.post = post_returning(payload)
    try:
        return wiki_ops._sql_scalar(COUNT_STMT)
    finally:
        wiki_ops.post = original_post


class RowsEnvelopeDiscipline(unittest.TestCase):
    """Аномальные конверты /api/query/sql — громкая ошибка, не пустой список."""

    def test_none_payload_raises_not_attribute_error(self):
        with self.assertRaises(RuntimeError) as cm:
            run_sql_rows(None)
        self.assertIn("аномальный конверт", str(cm.exception))

    def test_dict_without_data_raises_not_silent_empty(self):
        with self.assertRaises(RuntimeError) as cm:
            run_sql_rows({"code": 0})
        self.assertIn("аномальный конверт", str(cm.exception))

    def test_scalar_payload_raises(self):
        with self.assertRaises(RuntimeError) as cm:
            run_sql_rows(42)
        self.assertIn("аномальный конверт", str(cm.exception))

    def test_list_payload_returned_as_is(self):
        rows = [{"id": "20261106000000-abcdefg"}]
        self.assertEqual(run_sql_rows(rows), rows)

    def test_dict_with_list_data_is_unwrapped(self):
        rows = [{"hpath": "/Experience"}]
        self.assertEqual(run_sql_rows({"data": rows}), rows)


class ScalarContract(unittest.TestCase):
    """count-запрос обязан вернуть ровно одну строку; пустота — сбой ядра."""

    def test_empty_rows_raises_not_silent_zero(self):
        with self.assertRaises(RuntimeError) as cm:
            run_sql_scalar([])
        self.assertIn("0 строк", str(cm.exception))

    def test_count_row_converted_to_int(self):
        self.assertEqual(run_sql_scalar([{"c": 7}]), 7)


if __name__ == "__main__":
    unittest.main()
