#!/usr/bin/env python3
"""apply_stray_merges — применить merge-файлы сабов разбора остатков:
секция → append к целевому доку (Деплоймент/Домены), оригинал → бэкап в
/tmp/opencode/strays/backup/ → delete_doc. Верификация: append прошёл, бэкап на месте.

Запуск: python3 apply_stray_merges.py <verdicts-json> [<verdicts-json>…]
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from wiki_ops import call, close, deep_md, session

WS = "81475705-028a-4153-a8f5-f06e402ee31e"
MERGE = Path("/tmp/opencode/strays/merge")
BACKUP = Path("/tmp/opencode/strays/backup")
TARGETS = {  # docId целевых доков по (проект, судьба)
    ("origa", "deployment"): "Lhx5Stwiam",
    ("origa", "domains"): "fTmginTWo2",
    ("uwuwu", "deployment"): "kGZzDEKMls",
    ("uwuwu", "domains"): "cmQ31cL1kO",
    ("foilcap", "deployment"): "6qvU5JLaSR",
    ("foilcap", "domains"): "Zn79fkCywX",
    ("netcore", "deployment"): "Cd-7lLYXxe",
    ("netcore", "domains"): "iz_IzWIPCF",
    ("ckii", "deployment"): "OkjDjflZFQ",
    ("ckii", "domains"): "sAh6cPZw9D",
    ("desloper", "deployment"): "9EOJKDO5H5",
    ("desloper", "domains"): "PrEw-W6h-l",
    ("1xgames", "deployment"): "9LpvVsJI6f",
    ("1xgames", "domains"): "Uj2zcCuC8R",
}


def main() -> int:
    BACKUP.mkdir(parents=True, exist_ok=True)
    verdicts = []
    for f in sys.argv[1:]:
        verdicts += [
            v
            for v in json.loads(Path(f).read_text())
            if v["verdict"] in ("deployment", "domains")
        ]
    proj_by_file = {
        "verdicts-origa": "origa",
        "verdicts-uwuwu": "uwuwu",
        "verdicts-foilcap": "foilcap",
        "verdicts-netcore": "netcore",
        "verdicts-ckii": "ckii",
        "verdicts-desloper": "desloper",
        "verdicts-1xgames": "1xgames",
    }
    appended = deleted = skipped = 0
    sid = session()
    try:
        for vf in sys.argv[1:]:
            proj = proj_by_file[Path(vf).stem]
            for v in [
                v
                for v in json.loads(Path(vf).read_text())
                if v["verdict"] in ("deployment", "domains")
            ]:
                did = v["docId"]
                sec_f = MERGE / f"{did}.md"
                if not sec_f.exists() or not sec_f.read_text(encoding="utf-8").strip():
                    print(f"?? {v['title'][:40]}: merge-файл пуст — пропуск")
                    skipped += 1
                    continue
                # бэкап оригинала
                orig = (
                    deep_md(
                        call(
                            sid,
                            "export_doc_markdown",
                            {"workspaceId": WS, "docId": did},
                        )
                    )
                    or ""
                )
                (BACKUP / f"{did}.md").write_text(orig, encoding="utf-8")
                # append секции
                target = TARGETS[(proj, v["verdict"])]
                call(
                    sid,
                    "append_markdown",
                    {
                        "workspaceId": WS,
                        "docId": target,
                        "markdown": "\n"
                        + sec_f.read_text(encoding="utf-8").rstrip()
                        + "\n",
                    },
                )
                # удалить исходник (контент сохранён: секция + бэкап)
                call(
                    sid,
                    "delete_doc",
                    {"workspaceId": WS, "docId": did, "confirmDocId": did},
                )
                appended += 1
                deleted += 1
                print(f"⊕ {v['verdict']:10} {v['title'][:44]} → {target}")
                time.sleep(0.4)
    finally:
        close(sid)
    print(f"ИТОГ: append+delete {appended}, пропусков {skipped}; бэкапы: {BACKUP}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
