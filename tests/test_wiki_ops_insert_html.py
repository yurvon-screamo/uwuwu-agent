"""block-insert-html: dom-вставка живого HTML-блока обязана быть громкой.

Контракты (план задачи 20261107064314, ревью round-1/2; живые пробы 07.11):
  1) мутация — dataType="dom", previousID=якорь, один сетевой вызов (attempts=1);
  2) без маркера — паритет block-insert-after: формат id до SQL, doc-id — отказ;
  3) маркер-режим (ADR-052): маркер уникален в доке — 0 и >1 → отказ с раскладкой
     (SQL порядок детей не хранит: колонки sorting нет, sort у сиблингов
     одинаков — «первый по порядку» невыразим, детерминизм через уникальность);
     имя в SQL не подставляется вовсе (точный матч в питоне) — инъекция
     исключена архитектурно;
  4) сосед маркера — по .sy-файлу (FS-истина, паритет cli find_marker_location):
     NodeHTMLBlock → отказ до мутации с рецептом обновления (block-delete →
     повтор), иначе повторный вызов тихо плодит вторую диаграмму под маркером;
  5) пост-чек — два источника (ADR-025: один лагающий источник ≠ истина):
     SQL-проба новых id с ретраями (индекс отстаёт секундами), подтверждённый
     type≠'html' — громкий отказ; SQL не сошёлся → фолбэк .sy (NodeHTMLBlock
     по new_id — успех с примечанием); оба источника молчат → RuntimeError
     БЕЗ команды сноса (ложный негатив не должен привести агента к удалению
     реально вставленной диаграммы);
  6) data-content несёт HTML-эскейп с кавычками (атрибут в двойных кавычках);
  7) undo-журнал до мутации, new_ids — после; каждый SQL-stmt несёт явный LIMIT.

Мок-ядро материализует dom-id как живое ядро: insertBlock-эхо несёт id из dom,
индекс и .sy пополняются ими (insert_type деградации — по кейсу).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_insert_html.py
"""
import io
import json
import os
import re
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import tools.wiki_ops as wiki_ops

DOC_ID = "20261107070000-doc0001"
BLOCK_ID = "20261107070000-blk0001"
MARKER_ID = "20261107070000-mrk0001"
MARKER2_ID = "20261107070000-mrk0002"
SIBLING_ID = "20261107070000-sbl0001"
BOX = "20261103000000-box0001"
SY_PATH = "/20261103000000-rot0001/" + DOC_ID + ".sy"

DIAGRAM_HTML = '<div style="background:#101010"><span>probe</span></div>'


def _doc(doc_id=DOC_ID, hpath="/Projects/Scratch/html-insert-probe"):
    return {"id": doc_id, "type": "d", "root_id": "", "parent_id": "",
            "hpath": hpath, "box": BOX, "path": SY_PATH, "markdown": ""}


def _block(block_id=BLOCK_ID, content="Якорный абзац."):
    return {"id": block_id, "type": "p", "root_id": DOC_ID, "parent_id": DOC_ID,
            "content": content, "markdown": content}


def _marker(marker_id=MARKER_ID, name="arch"):
    return _block(marker_id, content=wiki_ops.MARKER_PREFIX + name)


def _sy_node(node_id, node_type):
    return {"ID": node_id, "Type": node_type}


def _sy_tree(html_sibling=None):
    """Дерево .sy до мутации: дети дока — якорь, маркер, (опционально) html-сосед
    сразу после маркера. Новые id дописывает мок при insertBlock."""
    tree = _doc()
    children = [
        _sy_node(BLOCK_ID, "NodeParagraph"),
        _sy_node(MARKER_ID, "NodeParagraph"),
    ]
    if html_sibling:
        children.append(_sy_node(SIBLING_ID, "NodeHTMLBlock"))
    tree["Children"] = children
    return tree


class InsertHtmlKernel:
    """Двойник kernel API для block-insert-html: blocks/docs-таблицы, SQL
    (id-resolve / маркеры like / id in (...)), insertBlock с материализацией
    dom-id в индекс и .sy (как живое ядро), getFile для .sy, putFile-журнал.

    insert_type — тип материализации в индексе ("html" — успех, "p" — деградация);
    sql_anomaly — деградация индекса на пост-чеке (id in (...)), резолв жив;
    sql_lag_probes — сколько первых пост-чек-проб отвечают пусто (лаг индекса);
    sy_materialize — дописывает ли мок dom-id в .sy (False — .sy противоречит).
    """

    def __init__(self, blocks=(), docs=(), insert_type="html", sql_anomaly=False,
                 sql_lag_probes=0, sy_tree=None, sy_missing=False, sy_materialize=True):
        self.blocks = {b["id"]: dict(b) for b in blocks}
        self.docs = {d["id"]: dict(d) for d in docs}
        self.insert_type = insert_type
        self.sql_anomaly = sql_anomaly
        self.sql_lag_probes = sql_lag_probes
        self.sy_tree = sy_tree
        self.sy_missing = sy_missing
        self.sy_materialize = sy_materialize
        self.sql_stmts = []
        self.sql_probes = 0
        self.mutations = []
        self.puts = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path == "/api/block/insertBlock":
            self.mutations.append((path, dict(body), attempts))
            return self._insert_block(body)
        raise AssertionError(f"неожиданный путь: {path}")

    def _insert_block(self, body: dict):
        # Живое ядро эхит вставленный dom в doOperations со своим id.
        dom_ids = re.findall(r'data-node-id="([^"]+)"', body.get("data", ""))
        data = "".join(f'<div data-node-id="{i}" data-type="NodeHTMLBlock"></div>'
                       for i in dom_ids)
        for i in dom_ids:
            self.blocks[i] = {"id": i, "type": self.insert_type, "root_id": DOC_ID,
                              "parent_id": DOC_ID, "content": "", "markdown": ""}
            if self.sy_tree is not None and self.sy_materialize:
                self.sy_tree.setdefault("Children", []).append(_sy_node(i, "NodeHTMLBlock"))
        return [{"timestamp": 0, "doOperations": [{"action": "insert", "data": data}]}]

    def get_file(self, ws_path: str) -> bytes:
        if self.sy_missing:
            raise FileNotFoundError(ws_path)
        return json.dumps(self.sy_tree).encode()

    def put_file(self, ws_path: str, payload: bytes):
        self.puts.append((ws_path, payload))

    def _sql(self, stmt: str):
        self.sql_stmts.append(stmt)
        rows = list(self.docs.values()) + list(self.blocks.values())
        if "id in (" in stmt:
            if self.sql_anomaly:
                raise RuntimeError("SQL вернул аномальный конверт")
            if self.sql_lag_probes > self.sql_probes:
                self.sql_probes += 1
                return []
            ids = re.findall(r"'([^']+)'", stmt)
            return [dict(r) for r in rows if r["id"] in ids]
        if "content like" in stmt:
            return [dict(r) for r in rows
                    if str(r.get("content", "")).startswith(wiki_ops.MARKER_PREFIX)]
        if "type='d'" in stmt:
            m = re.search(r"id='([^']+)'", stmt)
            doc = self.docs.get(m.group(1))
            return [dict(doc)] if doc else []
        m = re.search(r"where id='([^']+)' limit", stmt)
        row = (self.blocks.get(m.group(1)) or self.docs.get(m.group(1))) if m else None
        return [dict(row)] if row else []


def _kernel(**overrides) -> InsertHtmlKernel:
    base = dict(blocks=[_block(), _marker()], docs=[_doc()],
                sy_tree=_sy_tree(), sy_missing=False)
    base.update(overrides)
    return InsertHtmlKernel(**base)


def run_insert_html(kernel: InsertHtmlKernel, target: str, html_payload: str = DIAGRAM_HTML,
                    after_marker: str | None = None) -> str:
    f = tempfile.NamedTemporaryFile("w", suffix=".html", delete=False, encoding="utf-8")
    f.write(html_payload)
    f.close()
    args = SimpleNamespace(target=target, file=f.name, after_marker=after_marker)
    original = (wiki_ops.post, wiki_ops._put_workspace_file,
                wiki_ops._get_workspace_file, wiki_ops.time)
    wiki_ops.post = kernel.post
    wiki_ops._put_workspace_file = kernel.put_file
    wiki_ops._get_workspace_file = kernel.get_file
    wiki_ops.time = SimpleNamespace(sleep=lambda seconds: None, time=time.time)
    try:
        buffer = io.StringIO()
        with redirect_stdout(buffer):
            wiki_ops.cmd_block_insert_html(args)
    finally:
        (wiki_ops.post, wiki_ops._put_workspace_file,
         wiki_ops._get_workspace_file, wiki_ops.time) = original
        os.unlink(f.name)
    return buffer.getvalue()


def _dom_id(kernel: InsertHtmlKernel) -> str:
    _, body, _ = kernel.mutations[0]
    return re.search(r'data-node-id="([^"]+)"', body["data"]).group(1)


class HappyPath(unittest.TestCase):
    """Успешная dom-вставка: dataType="dom", previousID, журнал, вердикт по SQL."""

    def test_inserts_after_block_with_dom_and_reports_verdict(self):
        k = _kernel()
        out = run_insert_html(k, BLOCK_ID)
        path, body, attempts = k.mutations[0]
        self.assertEqual(path, "/api/block/insertBlock")
        self.assertEqual(body["dataType"], "dom")
        self.assertEqual(body["previousID"], BLOCK_ID)
        self.assertEqual(attempts, 1)
        self.assertIn('data-type="NodeHTMLBlock"', body["data"])
        self.assertIn(f"новый блок: {_dom_id(k)}", out)
        self.assertIn("вердикт по SQL", out)

    def test_data_content_carries_html_escape(self):
        k = _kernel()
        run_insert_html(k, BLOCK_ID, html_payload='<div a="1">x & y</div>')
        _, body, _ = k.mutations[0]
        self.assertIn('data-content="&lt;div a=&quot;1&quot;&gt;x &amp; y&lt;/div&gt;"',
                      body["data"])

    def test_journal_written_before_mutation_with_new_ids_after(self):
        k = _kernel()
        out = run_insert_html(k, BLOCK_ID)
        # Журнал пишется дважды: до мутации + патч new_ids после неё.
        self.assertEqual(len(k.puts), 2)
        first = json.loads(k.puts[0][1])
        patched = json.loads(k.puts[1][1])
        self.assertEqual(first["op"], "insert-html")
        self.assertEqual(first["payload_html"], DIAGRAM_HTML)
        self.assertNotIn("new_ids", first)
        self.assertEqual(patched["new_ids"], [_dom_id(k)])
        self.assertIn("undo-журнал", out)

    def test_updated_and_id_follow_kernel_format(self):
        k = _kernel()
        run_insert_html(k, BLOCK_ID)
        _, body, _ = k.mutations[0]
        self.assertRegex(body["data"],
                         r'data-node-id="\d{14}-[a-z0-9]{7}"')
        self.assertRegex(body["data"], r'updated="\d{14}"')


class DegradationPostcheck(unittest.TestCase):
    """Молчаливого параграфа быть не может: деградация — громкий отказ."""

    def test_confirmed_degradation_is_loud(self):
        k = _kernel(insert_type="p")
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, BLOCK_ID)
        self.assertIn("деградация вставки", str(ctx.exception))
        self.assertIn("block-delete", str(ctx.exception))

    def test_index_lag_retries_then_succeeds(self):
        k = _kernel(sql_lag_probes=1)
        out = run_insert_html(k, BLOCK_ID)
        self.assertIn("вердикт по SQL", out)
        self.assertEqual(k.sql_probes, 1)


class SyFallbackPostcheck(unittest.TestCase):
    """SQL не сошёлся — фолбэк на .sy; противоречие — отказ без команды сноса."""

    def test_sy_confirms_when_sql_anomalous(self):
        k = _kernel(sql_anomaly=True)
        out = run_insert_html(k, BLOCK_ID)
        self.assertIn("вердикт по .sy", out)

    def test_sy_contradiction_is_loud_without_delete_recipe(self):
        k = _kernel(sql_anomaly=True, sy_materialize=False)
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, BLOCK_ID)
        self.assertIn("не подтверждена", str(ctx.exception))
        self.assertIn("сверь export", str(ctx.exception))
        self.assertNotIn("block-delete", str(ctx.exception))

    def test_sy_file_missing_is_loud(self):
        k = _kernel(sql_anomaly=True, sy_missing=True)
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, BLOCK_ID)
        self.assertIn(".sy", str(ctx.exception))
        self.assertIn("uwuwu-cli doctor", str(ctx.exception))


class NoMarkerGuards(unittest.TestCase):
    """Без маркера — паритет block-insert-after."""

    def test_doc_id_without_marker_is_refused(self):
        k = _kernel()
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID)
        self.assertIn("doc-id", str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_malformed_id_rejected_before_sql(self):
        k = _kernel()
        with self.assertRaises(RuntimeError):
            run_insert_html(k, "garbage")
        self.assertEqual(k.sql_stmts, [])

    def test_missing_block_fails_before_mutation(self):
        k = _kernel()
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, "20261107070000-nope001")
        self.assertIn("не найден", str(ctx.exception))
        self.assertEqual(k.mutations, [])


class MarkerGuards(unittest.TestCase):
    """Маркер-режим: уникальность, сосед, инъекция."""

    def test_marker_missing_is_refused_with_layout(self):
        k = _kernel()
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="missing")
        self.assertIn("не найден", str(ctx.exception))
        self.assertIn("⁂ diagram:arch", str(ctx.exception))  # раскладка найденных

    def test_duplicate_markers_are_refused_with_occurrences(self):
        k = _kernel(blocks=[_block(), _marker(), _marker(MARKER2_ID)])
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="arch")
        self.assertIn("не уникален", str(ctx.exception))
        self.assertIn(MARKER_ID, str(ctx.exception))
        self.assertIn(MARKER2_ID, str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_html_sibling_refused_before_mutation_with_update_recipe(self):
        k = _kernel(sy_tree=_sy_tree(html_sibling=True))
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="arch")
        self.assertIn("уже вставлена", str(ctx.exception))
        self.assertIn("block-delete", str(ctx.exception))
        self.assertIn(SIBLING_ID, str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_paragraph_sibling_inserts_after_marker(self):
        k = _kernel()
        out = run_insert_html(k, DOC_ID, after_marker="arch")
        _, body, _ = k.mutations[0]
        self.assertEqual(body["previousID"], MARKER_ID)
        self.assertIn("вердикт по SQL", out)

    def test_marker_name_never_reaches_sql(self):
        # Имя матчится точно в питоне (SQL ищет только константный префикс) —
        # инъекция имени в stmt исключена архитектурно; кавычка не ломает stmt.
        k = _kernel()
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="it's")
        marker_stmts = [s for s in k.sql_stmts if "content like" in s]
        self.assertTrue(marker_stmts)
        self.assertNotIn("it's", marker_stmts[0])
        self.assertIn("не найден", str(ctx.exception))

    def test_block_target_resolves_to_its_doc(self):
        # В маркер-режиме blockID валиден: якорь ищется в доке блока.
        k = _kernel()
        out = run_insert_html(k, BLOCK_ID, after_marker="arch")
        _, body, _ = k.mutations[0]
        self.assertEqual(body["previousID"], MARKER_ID)
        self.assertIn("вердикт по SQL", out)


class SqlDiscipline(unittest.TestCase):
    """Каждый SQL-stmt несёт явный LIMIT (правило проекта)."""

    def test_every_sql_stmt_carries_explicit_limit(self):
        k = _kernel()
        run_insert_html(k, DOC_ID, after_marker="arch")
        self.assertTrue(k.sql_stmts)
        for stmt in k.sql_stmts:
            self.assertRegex(stmt.lower(), r"limit \d+")


if __name__ == "__main__":
    unittest.main()
