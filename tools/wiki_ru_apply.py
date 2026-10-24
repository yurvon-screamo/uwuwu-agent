#!/usr/bin/env python3
"""wiki_ru_apply — применить русифицированный markdown дока (для сабов).

wiki_ru_apply.py <docId> <файл-перевода.md>
- сверяет длину (перевод не должен терять контент: >55% оригинала)
- replace_doc_with_markdown по docId
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from triage_affine import WORKSPACE, mcp_call, mcp_result


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    doc_id, src = sys.argv[1], sys.argv[2]
    new = Path(src).read_text(encoding="utf-8")
    docs = Path("/tmp/opencode/wiki-audit/docs.json")
    orig = ""
    if docs.exists():
        import json
        orig = (json.loads(docs.read_text(encoding="utf-8")).get(doc_id) or {}).get("markdown") or ""
    if orig and len(new) < len(orig) * 0.55:
        print(f"!! перевод подозрительно короток: {len(new)} vs {len(orig)} — правь")
        return 2
    resp, sid = mcp_call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                        "clientInfo": {"name": "wiki-ru", "version": "1"}})
    mcp_result(resp)
    mcp_call("notifications/initialized", {}, sid, notify=True)
    resp, _ = mcp_call("tools/call", {"name": "replace_doc_with_markdown",
                                      "arguments": {"workspaceId": WORKSPACE, "docId": doc_id,
                                                    "markdown": new}}, sid)
    mcp_result(resp)
    print(f"ok: {doc_id} заменён ({len(new)} симв, было {len(orig)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
