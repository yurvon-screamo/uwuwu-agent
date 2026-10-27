#!/usr/bin/env python3
"""likec4_refresh — конвейер LikeC4 в афине (канон: сорцы в вики, вариант 2).

Источние модели — док AFFiNE с c4-фенсами (```c4 ... ```). Конвейер:
  1. export модель-дока → извлечь фенсы → tmp-папка/*.c4
  2. likec4 gen webcomponent (нативный CLI) → самодостаточный wc.mjs
  3. обёртка HTML (<likec4-view>) → вставка/замена эмбеда в целевом доке по маркеру
     ⁂ diagram:likec4-<view>

Запуск:
  python3 likec4_refresh.py <модель-docId> <целевой-docId> --view index [--title '…']
Замена: маркер должен существовать (создаётся при первом прогоне автоматически).
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from wiki_ops import call, close, deep_md, session

WS = "81475705-028a-4153-a8f5-f06e402ee31e"
LIKEC4_BIN = "likec4"  # нативный CLI (bun add -g likec4); фоллбэк — bunx likec4

HTML_TPL = """<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
body{{margin:0;background:#101010;font:13px/1.4 -apple-system,'Segoe UI',sans-serif}}
likec4-view{{display:block;width:100%;min-width:1200px;height:92vh;min-height:1000px}}
.hint{{color:#7E7E7E;font-size:11px;text-align:right;padding:2px 8px}}
</style>
</head>
<body>
<likec4-view view-id="{view}" color-scheme="dark"></likec4-view>
<div class="hint">likec4 · view: {view} · тап по ноде — детали, навигация по вью</div>
<script>
{js}
</script>
</body>
</html>
"""


def extract_c4(md: str) -> list[str]:
    """Извлечь блоки ```c4 … ``` из markdown модель-дока."""
    blocks = re.findall(r"``` ?c4\n(.*?)```", md, re.DOTALL)
    if not blocks:
        raise SystemExit("в модель-доке нет фенсов ```c4")
    return blocks


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("model_doc", help="docId дока-модели (фенсы ```c4)")
    p.add_argument("target_doc", help="docId целевого дока (эмбед)")
    p.add_argument(
        "--view", required=True, help="view-id из views.c4 (напр. index, deployment)"
    )
    p.add_argument("--title", default=None, help="подпись-заголовок над эмбедом (опц.)")
    a = p.parse_args()

    sid = session()
    try:
        # 1) сорцы из дока
        md = (
            deep_md(
                call(
                    sid,
                    "export_doc_markdown",
                    {"workspaceId": WS, "docId": a.model_doc},
                )
            )
            or ""
        )
        blocks = extract_c4(md)
        with tempfile.TemporaryDirectory(prefix="likec4-") as tmp:
            tmpd = Path(tmp)
            for i, b in enumerate(blocks):
                (tmpd / f"src-{i:02d}.c4").write_text(b, encoding="utf-8")
            print(f"извлечено фенсов: {len(blocks)}")
            # 2) генерация веб-компонента (нативный likec4 из PATH, без graphviz)
            cmd = (
                [LIKEC4_BIN] if shutil.which(LIKEC4_BIN) else ["bunx", "likec4@1.60.0"]
            )
            subprocess.run(
                [*cmd, "gen", "webcomponent", ".", "-o", "./wc.mjs", "--no-use-dot"],
                check=True,
                capture_output=True,
                text=True,
                cwd=tmp,
            )
            js = (tmpd / "wc.mjs").read_text(encoding="utf-8")
            if len(js) < 100_000:
                raise SystemExit(f"подозрительно малый wc.mjs: {len(js)}")
            html = HTML_TPL.format(view=a.view, js=js)
            print(f"wc.mjs: {len(js) // 1024}КБ → html {len(html) // 1024}КБ")

        # 3) маркер + эмбед в целевом доке
        marker = f"⁂ diagram:likec4-{a.view}"

        def dump():
            res = call(sid, "read_doc", {"workspaceId": WS, "docId": a.target_doc})

            def gt(o):
                if isinstance(o, dict):
                    for c in o.get("content", []):
                        if isinstance(c, dict) and "text" in c:
                            return c["text"]

            data = json.loads(gt(res) or "{}")
            blocks2 = data.get("blocks", [])
            note = next(b for b in blocks2 if b.get("flavour") == "affine:note")
            by_id = {b["id"]: b for b in blocks2}
            return note.get("childIds", []), by_id

        def btext(b):
            return "".join(d.get("insert", "") for d in (b.get("deltas") or [])) or (
                b.get("text") or ""
            )

        order, by_id = dump()
        m = next((cid for cid in order if btext(by_id.get(cid, {})) == marker), None)
        if not m:
            call(
                sid,
                "append_block",
                {
                    "workspaceId": WS,
                    "docId": a.target_doc,
                    "type": "paragraph",
                    "text": marker,
                },
            )
            time.sleep(0.5)
            order, by_id = dump()
            m = next(cid for cid in order if btext(by_id.get(cid, {})) == marker)
            print(f"+ маркер {marker} создан")
        i = order.index(m)
        if i + 1 < len(order) and "embed" in (
            by_id.get(order[i + 1], {}).get("flavour") or ""
        ):
            call(
                sid,
                "delete_block",
                {"workspaceId": WS, "docId": a.target_doc, "blockId": order[i + 1]},
            )
            time.sleep(0.4)
            print("старый эмбед снят")
        for attempt in range(3):
            try:
                call(
                    sid,
                    "append_block",
                    {
                        "workspaceId": WS,
                        "docId": a.target_doc,
                        "type": "embed_html",
                        "html": html,
                        "placement": {"afterBlockId": m},
                    },
                )
                print(f"⊕ эмбед likec4-{a.view} вставлен")
                break
            except SystemExit as e:
                print(f"ретрай {attempt + 1}: {str(e)[:60]}")
                time.sleep(120)
        else:
            raise SystemExit("не удалось вставить эмбед")
        return 0
    finally:
        close(sid)


if __name__ == "__main__":
    sys.exit(main())
