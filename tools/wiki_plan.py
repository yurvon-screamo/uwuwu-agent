#!/usr/bin/env python3
"""wiki_plan — планер чистки вики: инвентарь docs.json → план-файл для сабов.

Строит plan.json:
- links_todo: доки с текстовыми 'Смежное'-упоминаниями/битыми .md-линками →
  готовые замены [Заголовок](affine://doc/<id>) (резолв через affine-мапу
  slug→docId и kebab-нормализацию тайтлов)
- glue_candidates: пары доков на склейку (общие тайтл-префиксы, «part 2»,
  перекрытие упоминаний друг друга)
- ru_todo: не-русские доки (>500 симв) — очередь русификации
- rich_candidates: доки с '|' таблицами в тексте → нативная GFM-таблица

Запуск: python3 wiki_plan.py           — план в /tmp/opencode/wiki-audit/plan.json
        python3 wiki_plan.py --stat    — только сводка
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

WIKI = Path.home() / ".uwuwu"
AUDIT = Path("/tmp/opencode/wiki-audit")
DOCS = AUDIT / "docs.json"
PLAN = AUDIT / "plan.json"


def kebab(s: str) -> str:
    s = s.lower().strip()
    s = re.sub(r"[^a-z0-9]+", "-", s).strip("-")
    return s


def load_maps() -> dict[str, str]:
    """slug → docId из всех affine-мап."""
    out: dict[str, str] = {}
    for p in [WIKI / "affine_map.json", *WIKI.glob("affine_map_*.json")]:
        try:
            m = json.loads(p.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        for k, v in m.items():
            if not k.startswith("__") and isinstance(v, str):
                out.setdefault(k, v)
    return out


def build_plan() -> dict:
    docs = json.loads(DOCS.read_text(encoding="utf-8"))
    slug_map = load_maps()
    # прямые индексы: docId→title, kebab(title)→docId
    by_id = {did: d["title"] for did, d in docs.items() if not d.get("error")}
    by_kebab: dict[str, str] = {}
    for did, title in by_id.items():
        by_kebab.setdefault(kebab(title), did)

    def resolve(ref: str) -> tuple[str, str] | None:
        """ref (слаг или тайтл) → (docId, тайтл)."""
        ref = ref.strip().rstrip(".md").strip()
        k = kebab(ref)
        if not k:
            return None
        if ref in slug_map and slug_map[ref] in by_id:
            return slug_map[ref], by_id[slug_map[ref]]
        if k in by_kebab:
            return by_kebab[k], by_id[by_kebab[k]]
        # частичный: ref — префикс слага или слаг — префикс тайтла
        for slug, did in slug_map.items():
            if slug.startswith(k) and did in by_id:
                return did, by_id[did]
        return None

    links_todo, ru_todo, rich_candidates = [], [], []
    for did, d in docs.items():
        if d.get("error"):
            continue
        md = d.get("markdown") or ""
        title = d["title"]
        reps = []
        # 1) 'Смежное: a, b (пояснение)' — наши текстовые ссылки
        for m in re.finditer(r"Смежн\w+[:\s]+([^\n]{3,300})", md):
            blob = m.group(1)
            for ref in re.findall(r"`?([a-z0-9][a-z0-9/-]{3,60})`?", blob):
                if "/" in ref and not re.match(r"^[a-z0-9-]+/[a-z0-9-]+$", ref):
                    continue
                hit = resolve(ref)
                if hit:
                    reps.append({"from_ref": ref, "to": hit, "where": "smezhnoe"})
        # 2) битые .md-линки [x](...md)
        for m in re.finditer(r"\[([^\]]{1,80})\]\(([^)]+\.md)\)", md):
            label, path = m.group(1), m.group(2)
            ref = path.rsplit("/", 1)[-1][:-3]
            hit = resolve(ref) or resolve(label)
            if hit:
                reps.append(
                    {"from_ref": f"{label} → {path}", "to": hit, "where": "md_link"}
                )
        if reps:
            # дедуп по цели
            seen, uniq = set(), []
            for r in reps:
                key = (r["from_ref"], r["to"][0])
                if key not in seen:
                    seen.add(key)
                    uniq.append(r)
            links_todo.append({"docId": did, "title": title, "replacements": uniq})
        # рус
        if len(md) > 500 and len(re.findall(r"[а-яА-ЯёЁ]", md)) < len(md) * 0.05:
            ru_todo.append({"docId": did, "title": title, "len": len(md)})
        # rich: таблицо-подобные строки (| a | b |) без GFM-таблицы
        if not re.search(r"^\s*\|.*\|\s*$", md, re.MULTILINE) and re.search(
            r"^.{0,10}\|[^|\n]{2,40}\|[^|\n]{2,40}\|", md, re.MULTILINE
        ):
            rich_candidates.append({"docId": did, "title": title})

    # склейки: взаимные упоминания тайтлов + общий kebab-префикс слов 3+
    ids = list(by_id)
    glue = []
    title_words = {
        did: set(kebab(by_id[did]).split("-"))
        - {"the", "and", "a", "of", "in", "to", "for"}
        for did in ids
    }
    for i, a in enumerate(ids):
        for b in ids[i + 1 :]:
            common = title_words[a] & title_words[b]
            if len(common) >= 3:
                glue.append(
                    {
                        "a": {"docId": a, "title": by_id[a]},
                        "b": {"docId": b, "title": by_id[b]},
                        "common": sorted(common),
                    }
                )
    return {
        "links_todo": links_todo,
        "glue_candidates": glue,
        "ru_todo": ru_todo,
        "rich_candidates": rich_candidates,
        "stats": {
            "docs": len(docs),
            "link_docs": len(links_todo),
            "glue_pairs": len(glue),
            "ru": len(ru_todo),
            "rich": len(rich_candidates),
        },
    }


def main() -> int:
    if not DOCS.exists():
        print("нет docs.json — сначала wiki_audit.py fetch")
        return 2
    plan = build_plan()
    PLAN.write_text(json.dumps(plan, ensure_ascii=False, indent=1))
    print(json.dumps(plan["stats"], ensure_ascii=False))
    print(f"план: {PLAN}")
    if "--stat" not in sys.argv:
        print("\nтоп склеек по общим словам:")
        for g in sorted(plan["glue_candidates"], key=lambda x: -len(x["common"]))[:8]:
            print(
                f"  {g['a']['title'][:40]} + {g['b']['title'][:40]} ({','.join(g['common'][:5])})"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
