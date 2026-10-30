#!/usr/bin/env python3
"""wiki_ru_apply — применить русифицированный markdown дока (для сабов).

wiki_ru_apply.py <docId> <файл-перевода.md> [--stub]  # --stub: заглушка-редирект без длиновой защиты
- сверяет длину (перевод не должен терять контент: >55% оригинала)
- replace_doc_with_markdown по docId
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from triage_affine import WORKSPACE
from wiki_ops import call, close, session


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
    sid = session()
    try:
        # общая переиспользуемая сессия (файл разделяем с uwuwu-cli);
        # close() в finally не трогает общую — инцидент батча-00 (503 от
        # висящих сессий) закрыт структурно: сессий больше не плодим
        call(
            sid,
            "replace_doc_with_markdown",
            {
                "workspaceId": WORKSPACE,
                "docId": doc_id,
                "markdown": new,
            },
        )
    finally:
        close(sid)
    print(f"ok: {doc_id} заменён ({len(new)} симв, было {len(orig)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
