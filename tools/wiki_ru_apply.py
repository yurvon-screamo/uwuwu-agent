#!/usr/bin/env python3
"""wiki_ru_apply — применить русифицированный markdown дока (для сабов).

wiki_ru_apply.py <docId> <файл-перевода.md> [--stub]  # --stub: заглушка-редирект без длиновой защиты
- сверяет длину (перевод не должен терять контент: >55% оригинала)
- replace_doc_with_markdown по docId
"""

from __future__ import annotations

import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from triage_affine import MCP_TOKEN, MCP_URL, WORKSPACE, mcp_call, mcp_result


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

        orig = (json.loads(docs.read_text(encoding="utf-8")).get(doc_id) or {}).get(
            "markdown"
        ) or ""
    stub = "--stub" in sys.argv
    if orig and not stub and len(new) < len(orig) * 0.55:
        print(f"!! перевод подозрительно короток: {len(new)} vs {len(orig)} — правь")
        return 2
    resp, sid = mcp_call(
        "initialize",
        {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "wiki-ru", "version": "1"},
        },
    )
    try:
        mcp_result(resp)
        mcp_call("notifications/initialized", {}, sid, notify=True)
        resp, _ = mcp_call(
            "tools/call",
            {
                "name": "replace_doc_with_markdown",
                "arguments": {
                    "workspaceId": WORKSPACE,
                    "docId": doc_id,
                    "markdown": new,
                },
            },
            sid,
        )
        mcp_result(resp)
    finally:
        # шлюз держит висящие StreamableHTTP-сессии (~2 на клиента) и режет
        # новые initialize 503 — закрытие обязательно (инцидент батча-00)
        try:
            headers = {"mcp-session-id": sid}
            if MCP_TOKEN:
                headers["Authorization"] = f"Bearer {MCP_TOKEN}"
            req = urllib.request.Request(
                MCP_URL, data=b"", method="DELETE", headers=headers
            )
            urllib.request.urlopen(req, timeout=15).read()
        except urllib.error.URLError as e:
            # протухшая сессия умрёт по idle-timeout — не блокируем результат
            print(f"  warn: закрытие сессии не удалось ({e})", file=sys.stderr)
    print(f"ok: {doc_id} заменён ({len(new)} симв, было {len(orig)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
