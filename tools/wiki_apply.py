#!/usr/bin/env python3
"""wiki_apply — механическое применение линг-патчей из plan.json.

Для каждого links_todo-дока (rich-доки с ⚠️ исключены):
- живой export (не кэш!)
- патч A: в 'Смежное'-блоке голые слаги → [Заголовок](affine://doc/<id>)
- патч B: битые [label](path.md) → [label](affine://doc/<id>)
- замена через word-boundary; док с нулевыми изменениями не трогаем
- replace_doc_with_markdown; лог в apply-log.json

Запуск: python3 wiki_apply.py [n] [--yes]   — первые n доков (0 = все)
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from triage_affine import WORKSPACE, get_md
from wiki_ops import call, session

AUDIT = Path("/tmp/opencode/wiki-audit")
PLAN = AUDIT / "plan.json"
DOCS = AUDIT / "docs.json"
LOG = AUDIT / "apply-log.json"


def patch(markdown: str, reps: list[dict]) -> tuple[str, int]:
    """Применить замены; возвращает (новый markdown, число замен).

    A (md_link) приоритетен; цели, обработанные A, исключаются из B.
    На время B markdown-линки маскируются плейсхолдерами — слег не должен
    трогать label'ы линков (<'ollama' внутри [ollama-cloud](...)>).
    """
    n = 0
    # A: битые .md-линки [label](x.md) — точная замена целого линка
    for r in reps:
        if r["where"] != "md_link":
            continue
        label, path = r["from_ref"].split(" → ")
        target = f"[{label}](affine://doc/{r['to'][0]})"
        pat = re.compile(r"\[" + re.escape(label) + r"\]\(" + re.escape(path) + r"\)")
        markdown, k = pat.subn(target, markdown)
        n += k
    # B: голые слаги; линки маскируются — label'ы недоступны для слега
    slot = 0
    masks: dict[str, str] = {}

    def _mask(m: re.Match) -> str:
        nonlocal slot
        key = f"‹‹LINK{slot}››"
        slot += 1
        masks[key] = m.group(0)
        return key

    masked = re.sub(r"\[[^\]]*\]\([^)]+\)", _mask, markdown)
    for r in reps:
        if r["where"] != "smezhnoe":
            continue
        ref = r["from_ref"]
        target = f"[{r['to'][1]}](affine://doc/{r['to'][0]})"
        pat = re.compile(r"(?<![\w/-])" + re.escape(ref) + r"(?![\w/-])")
        masked, k = pat.subn(lambda _, t=target: t, masked, count=3)
        n += k
    for key, orig_link in masks.items():
        masked = masked.replace(key, orig_link)
    return masked, n


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("n", nargs="?", type=int, default=0)
    ap.add_argument("--yes", action="store_true")
    args = ap.parse_args()
    plan = json.loads(PLAN.read_text(encoding="utf-8"))
    docs = json.loads(DOCS.read_text(encoding="utf-8"))
    rich = {did for did, d in docs.items() if "⚠️" in (d.get("markdown") or "")}
    todo = [t for t in plan["links_todo"] if t["docId"] not in rich]
    if args.n:
        todo = todo[: args.n]
    print(
        f"линк-патчей: {len(todo)} (rich исключены: {len(plan['links_todo']) - len(todo)})",
        flush=True,
    )

    sid = session()

    log = json.loads(LOG.read_text()) if LOG.exists() else {}
    done = replaced = skipped = 0
    for t in todo:
        if t["docId"] in log and log[t["docId"]].get("status") == "ok":
            continue
        live = (
            get_md(
                call(
                    sid,
                    "export_doc_markdown",
                    {"workspaceId": WORKSPACE, "docId": t["docId"]},
                )
            )
            or ""
        )
        new, n = patch(live, t["replacements"])
        if n == 0:
            log[t["docId"]] = {
                "title": t["title"],
                "status": "no-match",
                "live_len": len(live),
            }
            skipped += 1
            continue
        print(f"  {t['title'][:50]}: {n} замен", flush=True)
        if not args.yes:
            log[t["docId"]] = {"title": t["title"], "status": "dry", "replacements": n}
            done += 1
            continue
        call(
            sid,
            "replace_doc_with_markdown",
            {
                "workspaceId": WORKSPACE,
                "docId": t["docId"],
                "markdown": new,
            },
        )
        log[t["docId"]] = {"title": t["title"], "status": "ok", "replacements": n}
        replaced += n
        done += 1
        LOG.write_text(json.dumps(log, ensure_ascii=False, indent=1))
        time.sleep(0.3)  # шлюз: не штурмуем
    LOG.write_text(json.dumps(log, ensure_ascii=False, indent=1))
    print(
        f"итог: доков {done}, замен {'dry' if not args.yes else replaced}, no-match {skipped}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
