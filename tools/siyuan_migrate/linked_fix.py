"""linked_fix.py — заменить embed-linked-doc комментарии на wikilinks.

Yjs даёт prop:pageId (линк на другой док). Контент линкнутого дока уже
мигрирован сам по себе → в SiYuan вместо `<!-- unsupported: ...embed-linked... -->`
ставим [[заголовок дока|трансклюзию]]: SiYuan поддерживает !{{...}}-вставки.
v1: обычные [[wikilinks]] (трансклюзию можно включить в UI).
"""
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from siyuan_migrate.siyuan_client import SiYuan


def main():
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    linked = json.loads(Path("/tmp/opencode/spike-affine/yjs/embeds-linked.json").read_text())
    manifest = json.loads((Path.home() / "siyuan-migration-work" / "export" / "manifest.json").read_text())
    from siyuan_migrate.boards import build_node_to_doc
    n2d = build_node_to_doc()

    # realDocId → title (по манифесту)
    id2title = {}
    for node, info in manifest["docs"].items():
        if info.get("ok"):
            id2title[n2d.get(node, node)] = Path(info["path"]).stem

    nb_cache = {nb["name"]: nb["id"] for nb in sy.ls_notebooks() if not nb.get("closed")}
    fixed = notfound = 0
    for doc_id, items in linked.items():
        meta_path = None
        for node, info in manifest["docs"].items():
            if info.get("ok") and n2d.get(node, node) == doc_id:
                p = Path(info["path"]).parts
                meta_path = (p[0], "/" + "/".join(p[1:-1]), Path(info["path"]).stem)
                break
        if not meta_path:
            continue
        nb_name, parent, title = meta_path
        nb = nb_cache.get(nb_name)
        if not nb:
            continue
        hpath = (parent.rstrip("/") + "/" + title) if parent != "/" else "/" + title
        doc = sy.get_doc_id_by_path(nb, hpath)
        if not doc:
            continue
        md = (sy._post("/api/export/exportMdContent", {"id": doc}) or {}).get("content", "")
        if not md:
            continue
        changed = 0
        for it in items:
            bid, pid = it["blockId"], it["pageId"]
            t = id2title.get(pid, "")
            repl = f"[[{t}]]" if t else f"*(мигрированный embed → doc {pid})*"
            for marker in (f"<!-- unsupported: flavour=affine:embed&#45;linked&#45;doc blockId={bid} -->",
                           f"<!-- unsupported: flavour=affine:embed-linked-doc blockId={bid} -->"):
                if marker in md:
                    md = md.replace(marker, repl)
                    changed += 1
                    break
        if changed:
            try:
                sy._post("/api/filetree/removeDocByID", {"id": doc})
                time.sleep(1)
                sy.create_doc_with_md(nb, hpath, md)
                fixed += 1
                print(f"линки: {hpath[:60]} ({changed} шт)")
            except Exception as e:
                print("FAIL", hpath[:40], str(e)[:60])
        time.sleep(0.4)
    print(f"DONE fixed={fixed} notfound={notfound}")


if __name__ == "__main__":
    main()
