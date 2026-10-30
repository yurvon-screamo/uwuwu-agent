#!/usr/bin/env python3
"""wiki_audit — инвентаризация воркспейса AFFiNE для чистки статей.

Выкачивает дерево + markdown всех доков ЖИВЬЁМ (MCP export, кэш ненадёжен)
в /tmp/opencode/wiki-audit/docs.json (чекпоинт каждые 25), строит инвентарь:
- kind/папка/длина/язык-эвристика
- маркеры: 'Смежное'-блоки (текстовые ссылки на склейку линков), битые
  md-линки (..md), rich-хвост '⚠️' (не-markdown контент — replace опасен!)
- кандидаты на склейку: совпадающие тайтлы-префиксы

Запуск: python3 wiki_audit.py fetch   — выкачка (рестартуемая)
        python3 wiki_audit.py report  — инвентарь и кандидаты
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
from wiki_ops import call
from wiki_ops import session as wiki_ops_session

AUDIT = Path("/tmp/opencode/wiki-audit")
DOCS = AUDIT / "docs.json"
TREE = AUDIT / "tree.json"


def session() -> str:
    # общая переиспользуемая сессия wiki-ops (файл разделяем с uwuwu-cli):
    # initialize = sign-in афины 4-7с на каждый запуск — больше не платим
    return wiki_ops_session()


def cmd_fetch(_: argparse.Namespace) -> int:
    AUDIT.mkdir(parents=True, exist_ok=True)
    sid = session()
    res = call(
        sid,
        "list_workspace_tree",
        {"workspaceId": WORKSPACE, "depth": 3},
    )
    s = json.dumps(res)
    TREE.write_text(s)
    nodes = re.findall(r'\{"docId"\s*:\s*"([^"]+)"[^}]*?"title"\s*:\s*"([^"]*)"', s)
    # docId→title напрямую из tree-узлов (порядок полей может отличаться — запасной путь)
    if len(nodes) < 50:
        ids = re.findall(r'"docId"\s*:\s*"([^"]+)"', s)
        titles = re.findall(r'"title"\s*:\s*"([^"]*)"', s)
        nodes = list(zip(ids, titles))
    known = json.loads(DOCS.read_text()) if DOCS.exists() else {}
    todo = [(d, t) for d, t in nodes if d not in known]
    print(
        f"дерево: {len(nodes)} узлов, выкачано {len(known)}, осталось {len(todo)}",
        flush=True,
    )
    t0 = time.time()
    for i, (did, title) in enumerate(todo, 1):
        try:
            res = call(
                sid,
                "export_doc_markdown",
                {"workspaceId": WORKSPACE, "docId": did},
            )
            known[did] = {"title": title, "markdown": get_md(res) or ""}
        except SystemExit:
            print(f"  !! {did} ({title}): export отказал, пропускаю", flush=True)
            known[did] = {"title": title, "markdown": "", "error": True}
        if i % 25 == 0 or i == len(todo):
            DOCS.write_text(json.dumps(known, ensure_ascii=False))
            print(f"  {i}/{len(todo)} ({i / (time.time() - t0):.1f} док/с)", flush=True)
    DOCS.write_text(json.dumps(known, ensure_ascii=False))
    print(f"готово: {len(known)} доков в {DOCS}")
    return 0


def cmd_report(_: argparse.Namespace) -> int:
    docs = json.loads(DOCS.read_text())
    rows = []
    for did, d in docs.items():
        md = d.get("markdown") or ""
        rows.append(
            {
                "docId": did,
                "title": d["title"],
                "len": len(md),
                "rich": "⚠️" in md,
                "smezhnoe": bool(re.search(r"Смежн\w+|Related", md)),
                "md_links": sorted(set(re.findall(r"\]\(([^)]+\.md)\)", md))),
                "affine_links": len(re.findall(r"affine://doc/", md)),
                "ru": len(re.findall(r"[а-яА-ЯёЁ]", md)) > len(md) * 0.05,
                "error": d.get("error", False),
            }
        )
    (AUDIT / "inventory.json").write_text(
        json.dumps(rows, ensure_ascii=False, indent=1)
    )
    n = len(rows)
    rich = sum(r["rich"] for r in rows)
    smezh = sum(r["smezhnoe"] for r in rows)
    withlinks = sum(1 for r in rows if r["md_links"])
    aff = sum(r["affine_links"] for r in rows)
    notru = [r["title"] for r in rows if not r["ru"] and r["len"] > 500][:10]
    print(
        f"доков: {n} | rich(⚠️): {rich} | со 'Смежное': {smezh} | "
        f"с битыми .md-линками: {withlinks} | нативных affine-линков: {aff}"
    )
    print(f"не-русские (>500 симв): {len(notru)}: {notru[:5]}")
    # кандидаты на склейку: близкие тайтлы
    titles = {}
    for r in rows:
        if r["len"] > 200:
            titles.setdefault(r["title"].lower().split()[0], []).append(r["title"])
    dup_starts = {k: v for k, v in titles.items() if len(v) > 3}
    print(f"группы по первому слову тайтла (>3): {dict(list(dup_starts.items())[:8])}")
    return 0


def main() -> int:
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("fetch")
    sub.add_parser("report")
    args = p.parse_args()
    return {"fetch": cmd_fetch, "report": cmd_report}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
