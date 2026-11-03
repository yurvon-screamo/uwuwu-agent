#!/usr/bin/env python3
"""Контракт wiki_ops unlink + общий парсер _org_nodes.

unlink <docId> снимает organize-линк(и) дока (дедуп дублей-доков, задача
aeihhFRpNt): линк может быть в нескольких папках — снимаются все; линка
нет → rc=1; неожиданная форма ответа шлюза → McpError (не голый KeyError).
_org_nodes — общий парсер list_organize_nodes для orgls/org/unlink.

Без шлюза: session/call/close подменяются на моки. Запуск:
python3 tools/test_wiki_ops_unlink.py  (exit 0 = все зелёные)
"""

from __future__ import annotations

import argparse
import io
import json
import sys
import contextlib
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import wiki_ops  # noqa: E402
from triage_affine import McpError  # noqa: E402

RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, fn) -> None:
    try:
        fn()
        RESULTS.append((name, True, ""))
        print(f"ok   {name}")
    except Exception as e:
        RESULTS.append((name, False, repr(e)))
        print(f"FAIL {name}: {e!r}")


def mcp_response(nodes) -> dict:
    """Ответ шлюза по контракту list_organize_nodes: JSON в content[0].text."""
    return {
        "content": [
            {"type": "text", "text": json.dumps({"nodes": nodes}, ensure_ascii=False)}
        ]
    }


class FakeGateway:
    """Мок session/call/close: пишет вызовы, отдаёт заготовленный ответ."""

    def __init__(self, response, nodes=None):
        self.response = response
        self.calls: list[tuple[str, dict]] = []
        self._nodes = nodes  # если задан — ответ собирается из нод

    def session(self) -> str:
        return "fake-sid"

    def close(self, sid: str) -> None:
        pass

    def call(self, sid: str, name: str, args: dict) -> dict:
        self.calls.append((name, args))
        if name == "list_organize_nodes" and self._nodes is not None:
            return mcp_response(self._nodes)
        return self.response


def run_unlink(fake: FakeGateway, doc_id: str) -> tuple[int, str, str]:
    wiki_ops.session = fake.session
    wiki_ops.close = fake.close
    wiki_ops.call = fake.call
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = wiki_ops.cmd_unlink(argparse.Namespace(docId=doc_id))
    return rc, out.getvalue(), err.getvalue()


def t_unlink_single_link() -> None:
    """Один линк → удаляется его nodeId, rc=0, в выводе parent."""
    nodes = [
        {"id": "node1", "parentId": "folderA", "type": "doc", "data": "Doc1"},
        {"id": "node2", "parentId": "folderB", "type": "doc", "data": "Other"},
    ]
    fake = FakeGateway({}, nodes=nodes)
    rc, out, err = run_unlink(fake, "Doc1")
    assert rc == 0, f"rc={rc}, err={err}"
    deletes = [a for name, a in fake.calls if name == "delete_organize_link"]
    assert deletes == [{"workspaceId": wiki_ops.WORKSPACE, "nodeId": "node1"}], deletes
    assert "unlinked node1" in out and "folderA" in out


def t_unlink_multiple_links() -> None:
    """Док в двух папках → снимаются оба линка."""
    nodes = [
        {"id": "n1", "parentId": "fA", "type": "doc", "data": "DocX"},
        {"id": "n2", "parentId": "fB", "type": "doc", "data": "DocX"},
    ]
    fake = FakeGateway({}, nodes=nodes)
    rc, out, _ = run_unlink(fake, "DocX")
    assert rc == 0
    deleted = [a["nodeId"] for name, a in fake.calls if name == "delete_organize_link"]
    assert sorted(deleted) == ["n1", "n2"], deleted


def t_unlink_not_found() -> None:
    """Линка нет → rc=1 + сообщение в stderr, delete не вызывается."""
    fake = FakeGateway({}, nodes=[{"id": "n", "type": "doc", "data": "Other"}])
    rc, out, err = run_unlink(fake, "Missing")
    assert rc == 1, f"rc={rc}"
    assert "линк дока не найден в organize" in err
    assert not [1 for name, _ in fake.calls if name == "delete_organize_link"]


def t_org_nodes_parses_contract() -> None:
    """_org_nodes достаёт nodes из content[0].text."""
    fake = FakeGateway(
        {}, nodes=[{"id": "i", "parentId": "p", "type": "folder", "title": "x"}]
    )
    wiki_ops.call = fake.call
    nodes = wiki_ops._org_nodes("fake-sid")
    assert nodes[0]["id"] == "i" and nodes[0]["type"] == "folder", nodes


def t_org_nodes_unexpected_shape_raises_mcperror() -> None:
    """Ответ без nodes (или пустой) → McpError, не KeyError."""
    for bad in ({}, {"content": []}, {"content": [{"type": "text"}]}):
        fake = FakeGateway(bad)
        wiki_ops.call = fake.call
        try:
            wiki_ops._org_nodes("fake-sid")
        except McpError as e:
            assert "нет nodes" in str(e), str(e)
        else:
            raise AssertionError(f"McpError не поднят для ответа {bad}")


def main() -> int:
    check("unlink: одиночный линк удаляется, parent в выводе", t_unlink_single_link)
    check("unlink: мульти-линк — снимаются все", t_unlink_multiple_links)
    check("unlink: линк не найден → rc=1, без delete", t_unlink_not_found)
    check("_org_nodes: парсинг content[0].text", t_org_nodes_parses_contract)
    check("_org_nodes: неожиданная форма → McpError", t_org_nodes_unexpected_shape_raises_mcperror)
    failed = [r for r in RESULTS if not r[1]]
    print(f"итого: {len(RESULTS) - len(failed)}/{len(RESULTS)} ok")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
