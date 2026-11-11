"""Блочная хирургия обязана быть безопасной: guard'ы до мутации, один сетевой
вызов, undo-журнал до мутации.

Контракты (живые пробы на реальном ядре 06.11, задача 20261104230713):
  1) blockID валидируется по формату SiYuan до ЛЮБОГО SQL (инъекция/мусор — отказ);
  2) несуществующий блок — внятная ошибка, мутация не вызывается;
  3) doc-id (type='d') — отказ всем трём командам: updateBlock на doc-id молча
     стирает весь док, правка/снос дока — doc-level append/replace/delete;
  4) multi-block payload для block-update — отказ: ядро молча применяет только
     первый блок, хвост исчезает без ошибки (проба 06.11); fenced-код держит
     пустые строки внутри одного блока;
  5) контейнер с детьми (list/blockquote) для block-delete — отказ: ядро сносит
     поддерево целиком и тихо (дети считаются по parent_id);
  6) каждая мутация — один сетевой вызов (attempts=1): авто-ретрай после потери
     ответа даёт дубль вставки;
  7) undo-журнал пишется ДО мутации через kernel putFile в
     data/assets/undo/wiki_ops/ (прямой FS-доступ с хоста невозможен: владелец —
     контейнер ядра), после insert дополняется new_ids; деградация журнала не
     валит операцию;
  8) каждый SQL-stmt несёт явный LIMIT (правило проекта после orgls-инцидента);
  9) блок type='t' — контекст всей таблицы (пробы 06.11/07.11): update с payload
     без separator-строки шапки (row-payload, не-табличный payload, fence с
     таблицей внутри) и delete — отказ до мутации и до undo-журнала; payload
     полной таблицы — осознанный rewrite (ядро переписывает ровно как задано);
     insert-after — сиблинг-инсерт после таблицы, без гварда;
 10) якорь-item (type='i') для insert-after принимает payload строго одного
     item'а: multi-item payload ядро молча обрезает до первого item'а с
     фантомными id в ответе (проба 07.11), не-item блок после item'а ядро
     отказывает громко; отказ гварда — до мутации и до undo-журнала;
 11) пост-чек материализации всех insert-after: напечатанные id сверяются
     с фактом (SQL с ретраями, фолбэк .sy — FS-истина); фантомные id —
     громкий отказ («напечатанные id» — не доказательство материала,
     проба 07.11); пустой new_ids в ответе ядра — громкий отказ;
     NodeList-обёртка markdown-парсера — транзиентна (ядро мержит item'ы
     в существующий list и отбрасывает её, проба 07.11) — не фантом;
 12) айтем (type='i') — контекст всего поддерева (живая проба 12.11,
     задача 20261111063638): update переписывает поддерево айтема payload'ом —
     штатный p-носитель обновляется каскадом, лишние чайлды (вложенный list,
     дубль-p qxob57e-класса) сносятся тихо, code=0; гвард: безопасны ноль
     чайлдов или ровно один p, иначе отказ до мутации и до undo-журнала;
     пост-чек insert-after: p-детей у вставленного айтема ≥ 2 — предупреждение
     (мутация уже применена, лишний p сносится block-delete).

Прогон: python3 -m unittest discover -s tests -t .  (из корня репо)
или напрямую: python3 tests/test_wiki_ops_blocks.py
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
from tests.wiki_ops_testkit import kernel_post

DOC_ID = "20261106030000-doc0001"
BLOCK_ID = "20261106030000-blk0001"
CHILD_ID = "20261106030000-chl0001"
LIST_ID = "20261106030000-lst0001"
ITEM_ID = "20261106030000-itm0001"
TABLE_ID = "20261106030000-tbl0001"
NEW_ID = "20261106040000-new0001"
NEW_ID_B = "20261106040000-new0002"
WRAP_ID = "20261106040000-wrp0001"

TABLE_MD = "| Колонка A | Колонка B |\n| --- | --- |\n| раз | один |\n| два | два |"

MULTI_BLOCK_PAYLOAD = "Первый абзац нового.\n\nВторой абзац — должен исчезнуть."


def _block(block_id=BLOCK_ID, btype="p", root_id=DOC_ID, parent_id=DOC_ID, markdown="Старый текст."):
    return {"id": block_id, "type": btype, "root_id": root_id,
            "parent_id": parent_id, "markdown": markdown}


BOX = "20261106030000"
SY_PATH = "/20261106030000-doc0001.sy"


def _doc(doc_id=DOC_ID, hpath="/Projects/Scratch/wiki-ops-probe"):
    return {"id": doc_id, "type": "d", "root_id": "", "parent_id": "",
            "hpath": hpath, "box": BOX, "path": SY_PATH, "markdown": ""}


class FakeKernel:
    """Двойник kernel API: blocks-таблица (id-резолв / дети по parent_id / hpath
    рута), блочные мутации с записью вызовов и attempts, putFile-журнал,
    getFile-проба .sy (sy_tree), параметризация ответа insertBlock (insert_ids —
    id в doOperations; materialize_ids — какие из них реально ложатся в blocks)."""

    def __init__(self, blocks=(), docs=(), put_fails=False,
                 insert_ids=(NEW_ID,), insert_types=None, materialize_ids=None, sy_tree=None,
                 insert_children=()):
        self.blocks = {b["id"]: dict(b) for b in blocks}
        self.docs = {d["id"]: dict(d) for d in docs}
        self.sql_stmts = []
        self.mutations = []
        self.puts = []
        self.put_fails = put_fails
        self.insert_ids = list(insert_ids)
        self.insert_types = dict(insert_types) if insert_types else {}
        self.materialize_ids = list(materialize_ids) if materialize_ids is not None else list(insert_ids)
        self.sy_tree = sy_tree
        # Дети, материализуемые вместе с вставкой (parent_id — первый insert_id):
        # пост-чек дубль-p у вставленных айтемов (qxob57e-класс, проба 11.11).
        self.insert_children = [dict(c) for c in insert_children]
        # Симуляция транспортного сбоя SQL после первой мутации: деградация
        # пост-чека (Common ревью раунда 1) не должна ронять успешную вставку.
        self.sql_fails_after_mutation = False

    def post(self, path: str, body: dict, attempts: int = 6):
        if path == "/api/query/sql":
            stmt = body["stmt"]
            if (self.sql_fails_after_mutation and self.mutations
                    and "parent_id" in stmt):
                # Точечно: сбой детской пробы пост-чека (qxob57e-контур) —
                # _verify_materialized остаётся живым, деградирует пост-чек.
                raise RuntimeError("POST /api/query/sql: симулированный транспортный сбой")
            return self._sql(stmt)
        if path in ("/api/block/updateBlock", "/api/block/insertBlock", "/api/block/deleteBlock"):
            self.mutations.append((path, dict(body), attempts))
            if path == "/api/block/insertBlock":
                # Форма ответа ядра: insertBlock отдаёт список транзакций (проба 06.11);
                # doOperations несут data-node-id напечатанных id — материализуются
                # не все (проба 07.11: multi-item payload → фантомные id; NodeList-
                # обёртка транзиентна — мерж в существующий list).
                data = "".join(f'<div data-node-id="{i}" data-type="{self.insert_types.get(i, "NodeParagraph")}"></div>'
                               for i in self.insert_ids)
                for i in self.materialize_ids:
                    self.blocks[i] = {"id": i, "type": "p", "root_id": DOC_ID,
                                      "parent_id": DOC_ID, "markdown": ""}
                if self.materialize_ids and self.insert_children:
                    for c in self.insert_children:
                        child = dict(c)
                        child.setdefault("parent_id", self.materialize_ids[0])
                        child.setdefault("root_id", DOC_ID)
                        self.blocks[child["id"]] = child
                return [{"timestamp": 0, "doOperations": [{"action": "insert", "data": data}]}]
            return {"doOperations": []}
        raise AssertionError(f"неожиданный путь: {path}")

    def get_file(self, ws_path: str) -> bytes:
        if self.sy_tree is None:
            raise FileNotFoundError(ws_path)
        return json.dumps(self.sy_tree).encode()

    def put_file(self, ws_path: str, payload: bytes):
        if self.put_fails:
            raise RuntimeError("putFile: ядро отказало")
        self.puts.append((ws_path, payload))

    def _sql(self, stmt: str):
        self.sql_stmts.append(stmt)
        all_rows = list(self.docs.values()) + list(self.blocks.values())
        if "id in (" in stmt:
            ids = re.findall(r"'([^']+)'", stmt)
            return [dict(r) for r in all_rows if r["id"] in ids]
        if "count(*)" in stmt:
            m = re.search(r"parent_id='([^']+)'", stmt)
            if m is None:
                raise AssertionError(f"непонятный stmt: {stmt}")
            return [{"c": sum(1 for r in all_rows if r.get("parent_id") == m.group(1))}]
        if "where parent_id='" in stmt:
            # Дети по parent_id (гвард айтемов update, пост-чек вставки):
            # select id, type, substr(markdown,…) md — строки блоков-детей,
            # алиас md эмулирует substr-проекцию живого ядра.
            m = re.search(r"parent_id='([^']+)'", stmt)
            rows = sorted((dict(r) for r in all_rows if r.get("parent_id") == m.group(1)),
                          key=lambda r: r["id"])
            if "substr(markdown" in stmt:
                for r in rows:
                    r["md"] = r.get("markdown", "")[:80]
            return rows
        if "type='d'" in stmt:
            m = re.search(r"id='([^']+)'", stmt)
            if m is None:
                raise AssertionError(f"непонятный stmt: {stmt}")
            doc = self.docs.get(m.group(1))
            return [dict(doc)] if doc else []
        m = re.search(r"where id='([^']+)' limit", stmt)
        if m is None:
            raise AssertionError(f"непонятный stmt: {stmt}")
        # Реальный индекс: одна таблица blocks для доков и блоков.
        row = self.blocks.get(m.group(1)) or self.docs.get(m.group(1))
        return [dict(row)] if row else []


def _journal_entries(kernel: FakeKernel) -> list[dict]:
    return [json.loads(payload) for _, payload in kernel.puts]


def run_block(kernel: FakeKernel, command: str, block_id: str = BLOCK_ID, payload: str | None = None) -> str:
    payload_file = None
    if payload is not None:
        f = tempfile.NamedTemporaryFile("w", suffix=".md", delete=False, encoding="utf-8")
        f.write(payload)
        f.close()
        payload_file = f.name
    args = SimpleNamespace(blockID=block_id, file=payload_file)
    fn = {"update": wiki_ops.cmd_block_update,
          "insert-after": wiki_ops.cmd_block_insert_after,
          "delete": wiki_ops.cmd_block_delete}[command]
    original = (wiki_ops._put_workspace_file, wiki_ops._get_workspace_file, wiki_ops.time)
    wiki_ops._put_workspace_file = kernel.put_file
    wiki_ops._get_workspace_file = kernel.get_file
    wiki_ops.time = SimpleNamespace(sleep=lambda seconds: None, time=time.time)
    try:
        buffer = io.StringIO()
        with kernel_post(kernel), redirect_stdout(buffer):
            fn(args)
    finally:
        (wiki_ops._put_workspace_file, wiki_ops._get_workspace_file, wiki_ops.time) = original
        if payload_file:
            os.unlink(payload_file)
    return buffer.getvalue()


class PayloadIsOneBlock(unittest.TestCase):
    """block-update принимает только одно-блочный payload — иначе тихая потеря хвоста."""

    def test_multi_block_payload_is_refused_and_mutation_not_called(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", payload=MULTI_BLOCK_PAYLOAD)
        self.assertIn("block-insert-after", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_fenced_code_with_blank_lines_is_one_block(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        payload = "```python\nx = 1\n\ny = 2\n\n```"
        run_block(kernel, "update", payload=payload)
        self.assertEqual(kernel.mutations[0][1]["data"], payload)

    def test_chunk_count_distinguishes_blank_split_and_fences(self):
        self.assertEqual(wiki_ops._payload_chunk_count("один\n\nдва"), 2)
        self.assertEqual(wiki_ops._payload_chunk_count("параграф\nс переносом"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("```py\n\n\nкод\n\n```"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("~~~\n\nкод\n~~~"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("- item один\n- item два"), 1)

    def test_adjacent_block_starts_counted_as_separate_blocks(self):
        # Проба ревью 05.11: ядро режет payload на блоки без пустой строки и
        # молча выбрасывает хвост — парсер обязан видеть те же границы.
        self.assertEqual(wiki_ops._payload_chunk_count("текст абзаца\n- пункт один\n- пункт два"), 2)
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n```python\nx = 1\n```\nхвостик"), 3)
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n# Заголовок"), 2)
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n> цитата"), 2)
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n2. нумерованный"), 2)
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n| a | b |"), 2)
        # `---` под абзацем — setext-заголовок (1 блок) + «хвост» новым абзацем.
        self.assertEqual(wiki_ops._payload_chunk_count("текст\n---\nхвост"), 2)

    def test_same_kind_and_nested_lines_stay_one_block(self):
        self.assertEqual(wiki_ops._payload_chunk_count("- item\n  - вложенный"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("> строка раз\n> строка два"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("| a | b |\n|---|---|\n| 1 | 2 |"), 1)
        self.assertEqual(wiki_ops._payload_chunk_count("- item\n    продолжение с отступом"), 1)

    def test_adjacent_list_payload_is_refused_at_command_level(self):
        # Живой кейс ревью: guard пропускал, updateBlock терял список тихо.
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", payload="текст абзаца\n- пункт один")
        self.assertIn("блока(ов)", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_empty_payload_is_refused(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", payload="   \n")
        self.assertIn("пустой payload", str(cm.exception))
        self.assertEqual(kernel.mutations, [])


class ContainerDeleteGuard(unittest.TestCase):
    """Контейнер с детьми сносу не подлежит: ядро унесёт поддерево целиком и тихо."""

    def test_list_container_with_children_is_refused(self):
        kernel = FakeKernel(
            blocks=[_block(LIST_ID, btype="l", markdown="- item один\n- item два"),
                    _block(CHILD_ID, btype="i", parent_id=LIST_ID, markdown="- item один")],
            docs=[_doc()],
        )
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "delete", block_id=LIST_ID)
        self.assertIn("контейнер", str(cm.exception))
        self.assertIn("1 дет", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_leaf_block_is_deleted(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        run_block(kernel, "delete")
        self.assertEqual(kernel.mutations,
                         [("/api/block/deleteBlock", {"id": BLOCK_ID}, 1)])


class DocIdIsNotABlock(unittest.TestCase):
    """doc-id (type='d') — отказ всем трём командам, всегда (update на doc-id
    молча стирает весь контент дока)."""

    def test_update_on_doc_id_is_refused(self):
        kernel = FakeKernel(blocks=[], docs=[_doc()])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=DOC_ID, payload="Один блок.")
        self.assertIn("doc-id не блок", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_insert_after_doc_id_is_refused(self):
        kernel = FakeKernel(blocks=[], docs=[_doc()])
        with self.assertRaises(RuntimeError):
            run_block(kernel, "insert-after", block_id=DOC_ID, payload="Один блок.")
        self.assertEqual(kernel.mutations, [])

    def test_delete_doc_id_is_refused(self):
        kernel = FakeKernel(blocks=[], docs=[_doc()])
        with self.assertRaises(RuntimeError):
            run_block(kernel, "delete", block_id=DOC_ID)
        self.assertEqual(kernel.mutations, [])


class TableBlockGuard(unittest.TestCase):
    """type='t' — контекст всей таблицы (пробы ядра 06.11/07.11, задача
    20261106085434): update с payload без separator-строки шапки переписывает
    таблицу целиком payload'ом (схлопывание, code=0), delete сносит её целиком —
    оба отказа до мутации и до undo-журнала. Полная таблица — осознанный rewrite
    (ядро переписывает ровно как задано, проба 07.11); insert-after — сиблинг
    после таблицы, доброкачественный."""

    def _table_kernel(self) -> FakeKernel:
        return FakeKernel(blocks=[_block(TABLE_ID, btype="t", markdown=TABLE_MD)], docs=[_doc()])

    def test_row_like_payload_update_is_refused_before_mutation_and_journal(self):
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=TABLE_ID, payload="| раз | правка |")
        self.assertIn("таблица", str(cm.exception))
        self.assertEqual(kernel.mutations, [])
        self.assertEqual(kernel.puts, [])

    def test_data_row_with_hyphen_is_not_mistaken_for_separator(self):
        # Ревью плана 07.11: наивный паттерн матчит дефис в data-строке
        # («| 2026-11-07 | отчёт |») и молча пропускает схлопывание.
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError):
            run_block(kernel, "update", block_id=TABLE_ID, payload="| 2026-11-07 | отчёт |")
        self.assertEqual(kernel.mutations, [])

    def test_non_table_payload_update_is_refused(self):
        # Не-табличный одно-блочный payload переписал бы таблицу параграфом —
        # конверсия 't' не через block-update (ревью плана 07.11).
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=TABLE_ID, payload="Просто параграф.")
        self.assertIn("таблица", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_fenced_table_syntax_payload_is_refused(self):
        # Separator внутри code-fence — не таблица: это конверсия в код-блок.
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError):
            run_block(kernel, "update", block_id=TABLE_ID,
                      payload="```markdown\n| a | b |\n| --- | --- |\n```")
        self.assertEqual(kernel.mutations, [])

    def test_tilde_fenced_table_syntax_payload_is_refused(self):
        # Ревью кода 07.11: FENCE_RE ловил только backticks — ~~~-фенс с
        # таблицей внутри проходил гвардом и молча заменял таблицу код-блоком.
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError):
            run_block(kernel, "update", block_id=TABLE_ID,
                      payload="~~~\n| a | b |\n| --- | --- |\n~~~")
        self.assertEqual(kernel.mutations, [])

    def test_full_table_payload_update_is_allowed(self):
        kernel = self._table_kernel()
        new_table = "| Новая шапка | Ц2 |\n| --- | --- |\n| альфа | 1 |"
        run_block(kernel, "update", block_id=TABLE_ID, payload=new_table)
        self.assertEqual(kernel.mutations,
                         [("/api/block/updateBlock",
                           {"dataType": "markdown", "data": new_table, "id": TABLE_ID}, 1)])

    def test_delete_on_table_block_is_refused(self):
        kernel = self._table_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "delete", block_id=TABLE_ID)
        self.assertIn("таблица", str(cm.exception))
        self.assertEqual(kernel.mutations, [])
        self.assertEqual(kernel.puts, [])

    def test_insert_after_table_block_is_allowed(self):
        kernel = self._table_kernel()
        run_block(kernel, "insert-after", block_id=TABLE_ID, payload="Абзац после таблицы.")
        self.assertEqual(kernel.mutations[0][0], "/api/block/insertBlock")
        self.assertEqual(kernel.mutations[0][1]["previousID"], TABLE_ID)


class BadInputs(unittest.TestCase):
    """Мусорный blockID и несуществующий блок: отказ до мутации."""

    def test_malformed_block_id_rejected_before_any_sql(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        with self.assertRaises(RuntimeError):
            run_block(kernel, "update", block_id="x' or 1=1--", payload="Один блок.")
        self.assertEqual(kernel.sql_stmts, [])
        self.assertEqual(kernel.mutations, [])

    def test_missing_block_fails_before_mutation(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "delete", block_id=CHILD_ID)
        self.assertIn("не найден", str(cm.exception))
        self.assertEqual(kernel.mutations, [])


class UpdateHappyPath(unittest.TestCase):
    """Одно-блочный update: payload ядру как есть, attempts=1, журнал до мутации."""

    def test_update_sends_payload_and_writes_journal(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        run_block(kernel, "update", payload="Новый текст блока.")
        self.assertEqual(kernel.mutations,
                         [("/api/block/updateBlock",
                           {"dataType": "markdown", "data": "Новый текст блока.", "id": BLOCK_ID}, 1)])
        entries = _journal_entries(kernel)
        self.assertEqual(len(entries), 1)
        journal_path, _ = kernel.puts[0]
        self.assertTrue(journal_path.startswith("data/assets/undo/wiki_ops/"))
        self.assertTrue(journal_path.endswith(f"-update-{BLOCK_ID}.json"))
        entry = entries[0]
        self.assertEqual(entry["before_md"], "Старый текст.")
        self.assertEqual(entry["after_md"], "Новый текст блока.")
        self.assertEqual(entry["root_id"], DOC_ID)
        self.assertEqual(entry["hpath"], "/Projects/Scratch/wiki-ops-probe")


class InsertAfterHappyPath(unittest.TestCase):
    """Вставка после блока: previousID, новые id в выводе и в журнале."""

    def test_insert_uses_previous_id_and_reports_new_ids(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        output = run_block(kernel, "insert-after", payload="Вставленный блок.")
        self.assertEqual(kernel.mutations,
                         [("/api/block/insertBlock",
                           {"dataType": "markdown", "data": "Вставленный блок.", "previousID": BLOCK_ID}, 1)])
        self.assertIn(NEW_ID, output)
        entries = _journal_entries(kernel)
        self.assertEqual(len(entries), 2, "журнал: запись до мутации + дополнение new_ids")
        self.assertEqual(entries[0]["payload_md"], "Вставленный блок.")
        self.assertEqual(entries[1]["new_ids"], [NEW_ID])
        self.assertEqual(kernel.puts[0][0], kernel.puts[1][0], "файл журнала тот же")


class InsertAfterListItemGuard(unittest.TestCase):
    """Якорь-item (type='i') принимает payload строго одного item'а (проба
    07.11: multi-item payload ядро молча обрезает до первого с фантомными id
    в ответе; не-item блок после item'а ядро отказывает громко). Отказ —
    до мутации и до undo-журнала."""

    def _item_kernel(self, **overrides) -> FakeKernel:
        blocks = [_block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- якорь item"),
                  _block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- якорь item")]
        return FakeKernel(blocks=blocks, docs=[_doc()], **overrides)

    def test_multi_item_payload_is_refused_before_mutation_and_journal(self):
        kernel = self._item_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", block_id=ITEM_ID,
                      payload="- первый пункт\n- второй пункт")
        self.assertIn("по одному", str(cm.exception))
        self.assertEqual(kernel.mutations, [])
        self.assertEqual(kernel.puts, [])

    def test_non_item_payload_is_refused_with_recipe(self):
        kernel = self._item_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", block_id=ITEM_ID, payload="Параграф после item'а.")
        self.assertIn("list-контейнера", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_adjacent_text_and_list_payload_is_refused(self):
        kernel = self._item_kernel()
        with self.assertRaises(RuntimeError):
            run_block(kernel, "insert-after", block_id=ITEM_ID, payload="текст абзаца\n- пункт")
        self.assertEqual(kernel.mutations, [])

    def test_single_item_payload_is_allowed(self):
        kernel = self._item_kernel()
        run_block(kernel, "insert-after", block_id=ITEM_ID, payload="- один пункт")
        self.assertEqual(kernel.mutations[0][0], "/api/block/insertBlock")
        self.assertEqual(kernel.mutations[0][1]["previousID"], ITEM_ID)

    def test_item_with_nested_content_is_allowed(self):
        # Вложенные строки с отступом — не top-level item'ы: один item с
        # вложенным контентом валиден; странности ядра ловит пост-чек.
        kernel = self._item_kernel()
        run_block(kernel, "insert-after", block_id=ITEM_ID,
                  payload="- пункт\n  вложенное продолжение")
        self.assertEqual(len(kernel.mutations), 1)

    def test_fenced_code_payload_is_refused_as_non_item(self):
        # Код-блок после item'а — не item: ядро отказало бы invalid structure
        # громко, гвард отказывает до мутации с рецептом (вложение в item —
        # «- пункт» с код-блоком с отступом, это items=1 и легально).
        kernel = self._item_kernel()
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", block_id=ITEM_ID,
                      payload="```\n- раз\n- два\n```")
        self.assertIn("не item", str(cm.exception))
        self.assertEqual(kernel.mutations, [])


class InsertAfterMaterializationPostcheck(unittest.TestCase):
    """Пост-чек всех insert-after: напечатанные id сверяются с фактом
    (SQL-проба с ретраями, фолбэк .sy); фантомы — громкий отказ."""

    def test_phantom_ids_are_refused_loudly(self):
        # Ядро напечатало 2 id, материализовало 1 (проба 07.11) — отказ
        # со списком фантомов; мутация ровно одна (пост-чек не ретраит мутацию).
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()],
                            insert_ids=[NEW_ID, NEW_ID_B], materialize_ids=[NEW_ID],
                            sy_tree={"Children": []})
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", payload="Первый.\n\nВторой.")
        message = str(cm.exception)
        self.assertIn("не материализовались", message)
        self.assertIn(NEW_ID_B, message)
        self.assertEqual(len(kernel.mutations), 1)

    def test_all_ids_materialized_passes(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()],
                            insert_ids=[NEW_ID, NEW_ID_B])
        output = run_block(kernel, "insert-after", payload="Первый.\n\nВторой.")
        self.assertIn("вердикт по SQL", output)
        self.assertIn(NEW_ID_B, output)

    def test_empty_new_ids_is_refused(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()], insert_ids=[])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", payload="Один блок.")
        self.assertIn("не вернул новые id", str(cm.exception))
        self.assertEqual(len(kernel.mutations), 1)

    def test_sy_fallback_rescues_lagging_index(self):
        # SQL новых id не видит (лаг индекса), .sy-дерево содержит — успех
        # с примечанием о фолбэке, а не ложный отказ.
        sy_tree = {"Children": [{"ID": NEW_ID, "Type": "NodeParagraph"}]}
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()],
                            insert_ids=[NEW_ID], materialize_ids=[], sy_tree=sy_tree)
        output = run_block(kernel, "insert-after", payload="Один блок.")
        self.assertIn("вердикт по .sy", output)

    def test_merged_list_wrapper_is_not_a_phantom(self):
        # Живая проба 07.11: single item после item'а ядро МЕРЖИТ в существующий
        # list — NodeList-обёртка из markdown-парсера не материализуется, её
        # отсутствие не потеря; вставленный item+p материализованы → успех.
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()],
                            insert_ids=[NEW_ID, WRAP_ID],
                            insert_types={NEW_ID: "NodeListItem", WRAP_ID: "NodeList"},
                            materialize_ids=[NEW_ID], sy_tree={"Children": []})
        output = run_block(kernel, "insert-after", payload="- один пункт")
        self.assertIn("вердикт по SQL", output)
        self.assertNotIn("потеря вставки", output)

    def test_lost_item_behind_wrapper_is_still_a_phantom(self):
        # Баговый кейс: обёртка транзиентна, но второй item+p потеряны ядром —
        # отказ по фантомам остаётся.
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()],
                            insert_ids=[NEW_ID, NEW_ID_B, WRAP_ID],
                            insert_types={NEW_ID: "NodeListItem", NEW_ID_B: "NodeListItem",
                                          WRAP_ID: "NodeList"},
                            materialize_ids=[NEW_ID], sy_tree={"Children": []})
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "insert-after", payload="- первый\n- второй")
        self.assertIn(NEW_ID_B, str(cm.exception))


class ItemUpdateChildGuard(unittest.TestCase):
    """block-update айтема (type='i') переписывает его поддерево ровно payload'ом
    (живая проба 12.11, задача 20261111063638): штатный p-носитель обновляется
    каскадом, но лишние чайлды сносятся тихо, code=0 — вложенный list исчезает
    целиком, дубль-p (qxob57e-класс) теряет носителя или дублируется. Отказ —
    до мутации и до undo-журнала; безопасно: ноль чайлдов или ровно один p."""

    def _item_kernel(self, children) -> FakeKernel:
        blocks = [_block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- пункт"),
                  _block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- пункт"),
                  *children]
        return FakeKernel(blocks=blocks, docs=[_doc()])

    def test_item_with_nested_list_child_is_refused_before_mutation_and_journal(self):
        # Главный кейс пробы 12.11: update айтема с вложенным списком молча
        # снёс поддерево (чайлды исчезли из SQL и экспорта) при «обновлён».
        kernel = self._item_kernel(children=[
            _block("20261112023600-cr10001", btype="p", parent_id=ITEM_ID,
                   markdown="Родитель с вложенным"),
            _block("20261112023600-cr20001", btype="l", parent_id=ITEM_ID,
                   markdown="- Вложенный пункт 1\n- Вложенный пункт 2"),
        ])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=ITEM_ID, payload="- пункт после правки")
        message = str(cm.exception)
        self.assertIn("чайлд", message)
        self.assertIn("20261112023600-cr20001", message, "перечисление сносимого: id")
        self.assertIn("l", message, "перечисление сносимого: тип")
        self.assertIn("block-delete", message, "рецепт для дублей/лишних чайлдов")
        self.assertEqual(kernel.mutations, [])
        self.assertEqual(kernel.puts, [], "отказ до undo-журнала")

    def test_item_with_duplicate_paragraph_is_refused(self):
        # qxob57e-модель (проба 11.11): айтем с p-носителем и p-дублем —
        # update переписывает поддерево, тексты чайлдов теряются/смешиваются.
        kernel = self._item_kernel(children=[
            _block("20261112023700-cr10001", btype="p", parent_id=ITEM_ID,
                   markdown="Текст айтема"),
            _block("20261112023700-cr20001", btype="p", parent_id=ITEM_ID,
                   markdown="ДУБЛЬ-ПАРАГРАФ старым текстом"),
        ])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=ITEM_ID, payload="- новый текст")
        self.assertIn("2", str(cm.exception))
        self.assertEqual(kernel.mutations, [])

    def test_item_with_single_carrier_paragraph_is_updated(self):
        # Негатив: штатная структура [i → p-носитель] — обновление проходит
        # (проба 12.11: ядро каскадно обновляет носитель, правка исполнена).
        kernel = self._item_kernel(children=[
            _block("20261112023200-cr10001", btype="p", parent_id=ITEM_ID,
                   markdown="Текст пункта"),
        ])
        output = run_block(kernel, "update", block_id=ITEM_ID, payload="- новый текст")
        self.assertIn("обновлён", output)
        self.assertEqual(kernel.mutations,
                         [("/api/block/updateBlock",
                           {"dataType": "markdown", "data": "- новый текст", "id": ITEM_ID}, 1)])

    def test_item_without_children_is_updated(self):
        # Ноль чайлдов (вне инварианта ядра, но безопасно: нечего сносить).
        kernel = self._item_kernel(children=[])
        run_block(kernel, "update", block_id=ITEM_ID, payload="- новый текст")
        self.assertEqual(len(kernel.mutations), 1)

    def test_item_with_single_non_paragraph_child_is_refused(self):
        # Единственный чайлд не-p (вложенный list без p-носителя) — не штатная
        # структура: гвард требует ровно один p, а не «≤1 чайлда» — регрессия
        # условия в len(children) <= 1 пропустила бы этот кейс (tester-пин).
        kernel = self._item_kernel(children=[
            _block("20261112024100-cr10001", btype="l", parent_id=ITEM_ID,
                   markdown="- Вложенный пункт"),
        ])
        with self.assertRaises(RuntimeError) as cm:
            run_block(kernel, "update", block_id=ITEM_ID, payload="- пункт после правки")
        self.assertIn("чайлд", str(cm.exception))
        self.assertEqual(kernel.mutations, [], "отказ до мутации")
        self.assertEqual(kernel.puts, [], "отказ до undo-журнала")


class InsertedItemDuplicateChildPostcheck(unittest.TestCase):
    """Пост-чек вставки айтемов: p-детей у нового айтема ≥ 2 — qxob57e-класс
    «дубль-p» (проба 11.11: insertBlock с айтем-пейлоадом оставлял чайлда со
    старым текстом). Мутация уже применена — громкое предупреждение с рецептом,
    не отказ. Один p — штатный носитель текста пункта (проба 12.11); l-дети —
    вложенный контент payload'а, легитимны."""

    def test_inserted_item_with_extra_paragraph_warns(self):
        kernel = FakeKernel(
            blocks=[_block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- якорь"),
                    _block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- якорь")],
            docs=[_doc()],
            insert_ids=[NEW_ID],
            insert_types={NEW_ID: "NodeListItem"},
            insert_children=[
                {"id": "20261112023800-ch10001", "type": "p", "markdown": "Текст пункта"},
                {"id": "20261112023800-ch20001", "type": "p", "markdown": "ДУБЛЬ старым текстом"},
            ],
        )
        output = run_block(kernel, "insert-after", block_id=ITEM_ID, payload="- один пункт")
        self.assertIn("⚠", output)
        self.assertIn(NEW_ID, output)
        self.assertIn("20261112023800-ch20001", output, "перечисление подозрительных детей")
        self.assertIn("block-delete", output, "рецепт сноса дубля")

    def test_inserted_item_with_single_carrier_paragraph_is_silent(self):
        # Негатив: штатная пара [i → p] — предупреждения нет.
        kernel = FakeKernel(
            blocks=[_block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- якорь"),
                    _block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- якорь")],
            docs=[_doc()],
            insert_ids=[NEW_ID],
            insert_types={NEW_ID: "NodeListItem"},
            insert_children=[
                {"id": "20261112023800-ch10001", "type": "p", "markdown": "Текст пункта"},
            ],
        )
        output = run_block(kernel, "insert-after", block_id=ITEM_ID, payload="- один пункт")
        self.assertNotIn("⚠", output)

    def test_inserted_item_children_not_yet_in_index_stay_silent(self):
        # Лаг индекса: детей нет в blocks (индекс не доехал) — SQL-проба
        # честно пуста, пост-чек n/a, не ложная тревога.
        kernel = FakeKernel(
            blocks=[_block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- якорь"),
                    _block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- якорь")],
            docs=[_doc()],
            insert_ids=[NEW_ID],
            insert_types={NEW_ID: "NodeListItem"},
            materialize_ids=[NEW_ID],
            sy_tree={"Children": [{"ID": NEW_ID, "Type": "NodeListItem"}]},
        )
        output = run_block(kernel, "insert-after", block_id=ITEM_ID, payload="- один пункт")
        self.assertNotIn("⚠", output)

    def test_sql_failure_in_postcheck_degrades_to_warning_not_error(self):
        # Common ревью раунда 1: транспортный сбой SQL в пост-чеке ронял
        # успешную вставку (rc≠0 после применённой мутации) — агент-потребитель
        # мог повторить вставку (дубль). Канон _verify_materialized: сбой пробы
        # — деградация в предупреждение, вставка считается состоявшейся.
        kernel = FakeKernel(
            blocks=[_block(ITEM_ID, btype="i", parent_id=LIST_ID, markdown="- якорь"),
                    _block(LIST_ID, btype="l", parent_id=DOC_ID, markdown="- якорь")],
            docs=[_doc()],
            insert_ids=[NEW_ID],
            insert_types={NEW_ID: "NodeListItem"},
            insert_children=[
                {"id": "20261112023800-ch10001", "type": "p", "markdown": "Текст пункта"},
                {"id": "20261112023800-ch20001", "type": "p", "markdown": "ДУБЛЬ старым текстом"},
            ],
            sy_tree={"Children": [{"ID": NEW_ID, "Type": "NodeListItem"}]},
        )
        kernel.sql_fails_after_mutation = True
        output = run_block(kernel, "insert-after", block_id=ITEM_ID, payload="- один пункт")
        self.assertEqual(len(kernel.mutations), 1, "вставка применена")
        self.assertIn("вставлено", output, "успех операции не скрыт")
        self.assertIn("не выполнен", output, "пост-чек деградировал в предупреждение")
        self.assertIn("parent_id", output, "рецепт ручной сверки структуры")


class MutationIsSingleAttempt(unittest.TestCase):
    """Все три мутации идут одним сетевым вызовом (ретрай = дубль вставки)."""

    def test_every_block_mutation_runs_with_attempts_one(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        run_block(kernel, "update", payload="Один блок.")
        run_block(kernel, "insert-after", payload="Один блок.")
        run_block(kernel, "delete")
        self.assertTrue(kernel.mutations)
        for _, _, attempts in kernel.mutations:
            self.assertEqual(attempts, 1)


class UndoJournalDegradation(unittest.TestCase):
    """Отказ ядра на putFile: warning, но операция выполняется — деградация
    журнала не валит мутацию."""

    def test_unwritable_journal_does_not_stop_mutation(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()], put_fails=True)
        output = run_block(kernel, "delete")
        self.assertEqual(kernel.mutations,
                         [("/api/block/deleteBlock", {"id": BLOCK_ID}, 1)])
        self.assertIn("WARNING", output)


class SqlDiscipline(unittest.TestCase):
    """Каждый stmt блочных команд несёт явный LIMIT (правило проекта)."""

    def test_every_sql_stmt_carries_explicit_limit(self):
        kernel = FakeKernel(blocks=[_block()], docs=[_doc()])
        run_block(kernel, "update", payload="Один блок.")
        run_block(kernel, "delete")
        self.assertTrue(kernel.sql_stmts)
        for stmt in kernel.sql_stmts:
            self.assertIn(" limit", stmt, f"stmt без явного LIMIT: {stmt}")


if __name__ == "__main__":
    unittest.main()
