"""Резолв-контур блочных команд обязан различать «жив, лагает» и «не существует».

Контракты (план задачи 20261107000209, ревью round-1/2; живые пробы 07.11):
  1) uniform-лаг индекса: by-id пуст на первых пробах, строка появляется в
     бюджете докатывания (INDEX_CATCHUP_BACKOFF) → мутация проходит, ровно
     один вызов, undo-журнал записан;
  2) индекс не догнал за бюджет → различимый лаг-отказ (жив по дереву,
     root в тексте), 0 мутаций, 0 undo-записей;
  3) дерево тоже не знает id (getBlockInfo code −1: ErrSpecTooNew /
     ErrBoxUnindexed / GetBlock==nil) → absent-отказ «ни в SQL-индексе,
     ни в дереве ядра», 0 мутаций;
  4) прочие коды конверта (вкл. 3 ErrIndexing) → честный RuntimeError
     «не подтверждает и не отрицает» — занятость ≠ отсутствие (fail-closed);
     бесформенный code-0-конверт → RuntimeError (конверт-дисциплина PR #111);
     транспорт дерева → наверх, НЕ под видом «не найден»;
  5) doc-id через дерево (rootID == id) → «doc-id не блок»;
  6) хэппи-путь нулевой дельты: ряд найден сразу — проб дерева и снов нет;
  7) marker-флоу на свежем доке + свежем маркере: вход и док-проба
     анкор-резолва подтверждают док деревом, маркер-SQL докатывается тем же
     бюджетом → вставка проходит; маркер не появился → отказ с лаг-хинтом и
     раскладкой; неуникальность — прежняя раскладка;
  8) .sy-проба на лагающем док-ряде: box/path даёт дерево (getBlockInfo).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_resolve.py
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

DOC_ID = "20261107090000-doc0001"
BLOCK_ID = "20261107090000-blk0001"
MARKER_ID = "20261107090000-mrk0001"
MARKER2_ID = "20261107090000-mrk0002"
BOX = "20261103000000-box0001"
SY_PATH = "/20261103000000-rot0001/" + DOC_ID + ".sy"

DIAGRAM_HTML = '<div style="background:#101010"><span>probe</span></div>'


def _doc(doc_id=DOC_ID, hpath="/Projects/Scratch/resolve-probe"):
    return {"id": doc_id, "type": "d", "root_id": "", "parent_id": "",
            "hpath": hpath, "box": BOX, "path": SY_PATH, "markdown": ""}


def _block(block_id=BLOCK_ID, content="Старый текст."):
    return {"id": block_id, "type": "p", "root_id": DOC_ID, "parent_id": DOC_ID,
            "content": content, "markdown": content}


def _marker(marker_id, name="arch"):
    return {"id": marker_id, "type": "p", "root_id": DOC_ID, "parent_id": DOC_ID,
            "content": wiki_ops.MARKER_PREFIX + name, "markdown": wiki_ops.MARKER_PREFIX + name}


def _alive_info(doc_id=DOC_ID):
    return {"rootID": doc_id, "box": BOX, "path": SY_PATH}


class ResolveKernel:
    """Двойник kernel API резолв-контура: blocks/docs-таблицы (истина после
    флуша), структурный getBlockInfo-конверт, updateBlock/insertBlock/deleteBlock
    со счётчиками мутаций, putFile/getFile (undo, .sy).

    Uniform-лаг индекса честный: один глобальный бюджет sql_miss_probes —
    первые K SQL-проб ЛЮБОГО stmt отвечают пусто (один индекс, один флуш),
    данные в таблицах есть с самого начала. by_id_probes — отдельный счётчик
    by-id stmt для ассертов докатывания. envelopes — id → (code, data, msg)
    для getBlockInfo (нет записи → код −1, как живое ядро для неизвестного id).
    """

    def __init__(self, blocks=(), docs=(), sql_miss_probes=0,
                 envelopes=None, sy_tree=None):
        self.blocks = {b["id"]: dict(b) for b in blocks}
        self.docs = {d["id"]: dict(d) for d in docs}
        self.sql_miss_probes = sql_miss_probes
        self.envelopes = dict(envelopes or {})
        self.sy_tree = sy_tree
        self.sql_probes = 0
        self.by_id_probes = 0
        self.tree_probes = []
        self.mutations = []
        self.puts = []
        self.sleeps = []
        self.gets = []

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            return self._sql(body["stmt"])
        if path in ("/api/block/updateBlock", "/api/block/insertBlock",
                    "/api/block/deleteBlock"):
            self.mutations.append((path, dict(body), attempts))
            if path == "/api/block/updateBlock":
                return ""
            if path == "/api/block/deleteBlock":
                return ""
            return self._insert_block(body)
        raise AssertionError(f"неожиданный путь: {path}")

    def envelope(self, path: str, body: dict):
        self.tree_probes.append((path, body["id"]))
        code, data, msg = self.envelopes.get(
            body["id"], (-1, None, "Содержимое блока с id [...] не найдено"))
        return code, data, msg

    def get_file(self, ws_path: str) -> bytes:
        self.gets.append(ws_path)
        if self.sy_tree is None:
            raise FileNotFoundError(ws_path)
        return json.dumps(self.sy_tree).encode()

    def put_file(self, ws_path: str, payload: bytes):
        self.puts.append((ws_path, payload))

    def _insert_block(self, body: dict):
        # Живое ядро эхит вставленный dom в doOperations со своим id.
        dom_ids = re.findall(r'data-node-id="([^"]+)"', body.get("data", ""))
        data = "".join(f'<div data-node-id="{i}" data-type="NodeHTMLBlock"></div>'
                       for i in dom_ids)
        for i in dom_ids:
            self.blocks[i] = {"id": i, "type": "html", "root_id": DOC_ID,
                              "parent_id": DOC_ID, "content": "", "markdown": ""}
        return [{"timestamp": 0, "doOperations": [{"action": "insert", "data": data}]}]

    def _sql(self, stmt: str):
        self.sql_probes += 1
        if "where id='" in stmt and "limit 1" in stmt and "type='d'" not in stmt:
            self.by_id_probes += 1  # считается до бюджет-гейта: промахи тоже пробы
        if self.sql_miss_probes >= self.sql_probes:
            return []
        rows = list(self.docs.values()) + list(self.blocks.values())
        if "id in (" in stmt:
            ids = re.findall(r"'([^']+)'", stmt)
            return [dict(r) for r in rows if r["id"] in ids]
        if "content like" in stmt:
            return [dict(r) for r in rows
                    if str(r.get("content", "")).startswith(wiki_ops.MARKER_PREFIX)]
        if "count(*)" in stmt:
            m = re.search(r"parent_id='([^']+)'", stmt)
            children = [r for r in self.blocks.values() if r["parent_id"] == m.group(1)]
            return [{"c": len(children)}]
        if "type='d'" in stmt:
            m = re.search(r"id='([^']+)'", stmt)
            doc = self.docs.get(m.group(1))
            return [dict(doc)] if doc else []
        m = re.search(r"where id='([^']+)' limit 1", stmt)
        if m:
            row = self.blocks.get(m.group(1)) or self.docs.get(m.group(1))
            return [dict(row)] if row else []
        raise AssertionError(f"неожиданный stmt: {stmt}")


def _kernel(**overrides) -> ResolveKernel:
    base = dict(blocks=[_block()], docs=[_doc()])
    base.update(overrides)
    return ResolveKernel(**base)


def _run(kernel, cmd, args):
    original = (wiki_ops.post, wiki_ops._post_envelope, wiki_ops._put_workspace_file,
                wiki_ops._get_workspace_file, wiki_ops.time)
    wiki_ops.post = kernel.post
    wiki_ops._post_envelope = kernel.envelope
    wiki_ops._put_workspace_file = kernel.put_file
    wiki_ops._get_workspace_file = kernel.get_file
    wiki_ops.time = SimpleNamespace(sleep=kernel.sleeps.append, time=time.time)
    try:
        out = io.StringIO()
        with redirect_stdout(out):
            cmd(args)
    finally:
        (wiki_ops.post, wiki_ops._post_envelope, wiki_ops._put_workspace_file,
         wiki_ops._get_workspace_file, wiki_ops.time) = original
    return out.getvalue()


def _payload_file(text: str) -> str:
    f = tempfile.NamedTemporaryFile("w", suffix=".md", delete=False, encoding="utf-8")
    f.write(text)
    f.close()
    return f.name


def run_block_update(kernel, block_id, payload="Обновлённый текст."):
    f = _payload_file(payload)
    try:
        return _run(kernel, wiki_ops.cmd_block_update,
                    SimpleNamespace(blockID=block_id, file=f))
    finally:
        os.unlink(f)


def run_block_delete(kernel, block_id):
    return _run(kernel, wiki_ops.cmd_block_delete, SimpleNamespace(blockID=block_id))


def run_insert_html(kernel, target, after_marker=None, payload=DIAGRAM_HTML):
    f = tempfile.NamedTemporaryFile("w", suffix=".html", delete=False, encoding="utf-8")
    f.write(payload)
    f.close()
    args = SimpleNamespace(target=target, file=f.name, after_marker=after_marker)
    try:
        return _run(kernel, wiki_ops.cmd_block_insert_html, args)
    finally:
        os.unlink(f.name)


class BlockUpdateCatchup(unittest.TestCase):
    """Резолв блока: индекс → второе мнение дерева → докатывание."""

    def test_update_proceeds_after_index_catchup_on_uniform_lag(self):
        # (а) uniform-лаг: by-id пуст на первых двух SQL-пробах, строка на третьей.
        k = _kernel(sql_miss_probes=2, envelopes={BLOCK_ID: (0, _alive_info(), "")})
        run_block_update(k, BLOCK_ID)
        self.assertEqual(len(k.mutations), 1)
        path, body, attempts = k.mutations[0]
        self.assertEqual(path, "/api/block/updateBlock")
        self.assertEqual(body["id"], BLOCK_ID)
        self.assertEqual(attempts, 1)
        self.assertEqual(k.by_id_probes, 3)
        self.assertTrue(k.tree_probes)
        self.assertEqual(len(k.puts), 1)

    def test_lag_refusal_is_distinct_when_index_never_catches_up(self):
        k = _kernel(sql_miss_probes=99, envelopes={BLOCK_ID: (0, _alive_info(), "")})
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, BLOCK_ID)
        msg = str(ctx.exception)
        self.assertIn("жив", msg)
        self.assertIn("лаг индексации", msg)
        self.assertIn(DOC_ID, msg)
        self.assertNotIn("не найден", msg)
        self.assertEqual(k.mutations, [])
        self.assertEqual(k.puts, [])

    def test_absent_when_tree_also_misses(self):
        k = _kernel(sql_miss_probes=99)
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, BLOCK_ID)
        self.assertIn("ни в SQL-индексе, ни в дереве ядра", str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_busy_code_is_honest_error_not_absent(self):
        k = _kernel(sql_miss_probes=99,
                    envelopes={BLOCK_ID: (3, None, "indexing in progress")})
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, BLOCK_ID)
        msg = str(ctx.exception)
        self.assertIn("не подтверждает и не отрицает", msg)
        self.assertIn("3", msg)
        self.assertNotIn("не найден", msg)
        self.assertEqual(k.mutations, [])

    def test_shapeless_envelope_is_honest_error(self):
        k = _kernel(sql_miss_probes=99, envelopes={BLOCK_ID: (0, None, "")})
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, BLOCK_ID)
        self.assertIn("аномальный конверт", str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_transport_error_in_tree_probe_is_not_absent(self):
        k = _kernel(sql_miss_probes=99)

        def broken_envelope(path, body):
            raise RuntimeError("POST /api/block/getBlockInfo: сеть недоступна")

        k.envelope = broken_envelope
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, BLOCK_ID)
        self.assertNotIn("не найден", str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_happy_path_skips_tree_probe_and_sleeps(self):
        k = _kernel()
        run_block_update(k, BLOCK_ID)
        self.assertEqual(len(k.mutations), 1)
        self.assertEqual(k.tree_probes, [])
        self.assertEqual(k.sleeps, [])

    def test_doc_id_via_tree_refused_as_doc(self):
        k = _kernel(blocks=[], docs=[], sql_miss_probes=99,
                    envelopes={DOC_ID: (0, _alive_info(DOC_ID), "")})
        with self.assertRaises(RuntimeError) as ctx:
            run_block_update(k, DOC_ID)
        self.assertIn("doc-id не блок", str(ctx.exception))
        self.assertEqual(k.mutations, [])

    def test_block_delete_uses_same_resolve(self):
        k = _kernel(sql_miss_probes=2, envelopes={BLOCK_ID: (0, _alive_info(), "")})
        run_block_delete(k, BLOCK_ID)
        self.assertEqual(len(k.mutations), 1)
        path, _, _ = k.mutations[0]
        self.assertEqual(path, "/api/block/deleteBlock")
        self.assertEqual(k.by_id_probes, 3)


class InsertHtmlMarkerResolve(unittest.TestCase):
    """Marker-флоу: вход, док-проба анкора и маркер-SQL переживают лаг."""

    def _sy_with_marker(self, *marker_ids):
        children = [{"ID": i, "Type": "NodeParagraph"} for i in marker_ids]
        return {"ID": DOC_ID, "Children": children}

    def test_fresh_doc_and_fresh_marker_flow_proceeds_after_catchup(self):
        # (б) uniform-лаг: док-ряд и маркер-ряд невидимы первые три SQL-пробы
        # (entry by-id, док-проба анкора, маркер-SQL), маркер докатывается на
        # четвёртой — вставка проходит; вход и анкор подтверждают док деревом.
        k = _kernel(blocks=[_marker(MARKER_ID)], docs=[_doc()], sql_miss_probes=3,
                    envelopes={DOC_ID: (0, _alive_info(), "")},
                    sy_tree=self._sy_with_marker(MARKER_ID))
        run_insert_html(k, DOC_ID, after_marker="arch")
        self.assertEqual(len(k.mutations), 1)
        path, body, attempts = k.mutations[0]
        self.assertEqual(path, "/api/block/insertBlock")
        self.assertEqual(body["previousID"], MARKER_ID)
        self.assertEqual(attempts, 1)
        self.assertEqual(len(k.puts), 2)

    def test_marker_refusal_mentions_lag_when_never_visible(self):
        k = _kernel(blocks=[], docs=[], sql_miss_probes=99,
                    envelopes={DOC_ID: (0, _alive_info(), "")},
                    sy_tree=self._sy_with_marker())
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="arch")
        msg = str(ctx.exception)
        self.assertIn("не найден в доке", msg)
        self.assertIn("лаг индексации", msg)
        self.assertIn("маркеры в доке", msg)
        self.assertEqual(k.mutations, [])
        self.assertEqual(k.puts, [])

    def test_marker_uniqueness_listing_preserved(self):
        k = _kernel(blocks=[_marker(MARKER_ID), _marker(MARKER2_ID)], docs=[_doc()],
                    sy_tree=self._sy_with_marker(MARKER_ID, MARKER2_ID))
        with self.assertRaises(RuntimeError) as ctx:
            run_insert_html(k, DOC_ID, after_marker="arch")
        self.assertIn("не уникален", str(ctx.exception))
        self.assertEqual(k.mutations, [])


class SyTreeOnLag(unittest.TestCase):
    """.sy-проба переживает лагающий док-ряд: box/path даёт дерево."""

    def test_sy_tree_falls_back_to_tree_box_path(self):
        k = _kernel(blocks=[], docs=[], sql_miss_probes=1,
                    envelopes={DOC_ID: (0, _alive_info(), "")},
                    sy_tree={"ID": DOC_ID})
        original = (wiki_ops._post_envelope, wiki_ops._get_workspace_file)
        wiki_ops._post_envelope = k.envelope
        wiki_ops._get_workspace_file = k.get_file
        try:
            tree = wiki_ops._sy_tree(DOC_ID)
        finally:
            (wiki_ops._post_envelope, wiki_ops._get_workspace_file) = original
        self.assertEqual(k.gets, [f"data/{BOX}{SY_PATH}"])
        self.assertEqual(tree["ID"], DOC_ID)

    def test_sy_tree_refuses_distinctly_when_tree_misses(self):
        k = _kernel(blocks=[], docs=[], envelopes={})
        original = (wiki_ops._post_envelope, wiki_ops._get_workspace_file)
        wiki_ops._post_envelope = k.envelope
        wiki_ops._get_workspace_file = k.get_file
        try:
            with self.assertRaises(RuntimeError) as ctx:
                wiki_ops._sy_tree(DOC_ID)
        finally:
            (wiki_ops._post_envelope, wiki_ops._get_workspace_file) = original
        self.assertIn("ни в SQL-индексе, ни в дереве ядра", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
