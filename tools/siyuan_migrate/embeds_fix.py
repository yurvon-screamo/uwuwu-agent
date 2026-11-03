"""embeds_fix.py — вернуть html_embed-диаграммы в смигрированные доки SiYuan.

Источник: yjs/embeds-html.json (prop:html из Yjs-снапшотов афины).
В SiYuan: экспорт дока → заменить `<!-- unsupported: ... blockId=X -->` на
```html-фенс (SiYuan рендерит HTML-блоки) → пересоздать док по тому же hPath.
"""
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from siyuan_migrate.siyuan_client import SiYuan

MANIFEST = Path.home() / "siyuan-migration-work" / "export" / "manifest.json"


def build_paths():
    """realDocId → (notebook, hpath) из манифеста + организ-дерева (node→docId)."""
    mf = json.loads(MANIFEST.read_text())
    from siyuan_migrate.boards import build_node_to_doc
    n2d = build_node_to_doc()  # node_id → real docId (нужна живая афина)
    paths = {}
    for node_id, info in mf["docs"].items():
        if not info.get("ok"):
            continue
        real = n2d.get(node_id, node_id)
        p = Path(info["path"]).parts  # (Top, ..., 'x.md')
        paths[real] = (p[0], "/" + "/".join(p[1:-1]), Path(info["path"]).stem)
    return paths


def main():
    embeds = json.loads(Path("/tmp/opencode/spike-affine/yjs/embeds-html.json").read_text())
    sy = SiYuan.from_workspace_conf(Path.home() / "siyuan-demo-ws")
    paths = build_paths()
    nbs = {nb["name"]: nb["id"] for nb in sy.ls_notebooks() if not nb.get("closed")}

    fixed = skipped = notfound = 0
    for doc_id, items in embeds.items():
        meta = paths.get(doc_id)
        if not meta:
            print("нет пути в манифесте:", doc_id)
            notfound += 1
            continue
        nb_name, parent, title = meta
        nb = nbs.get(nb_name)
        if not nb:
            print("нет ноутбука:", nb_name)
            notfound += 1
            continue
        hpath = (parent.rstrip("/") + "/" + title) if parent != "/" else "/" + title
        # найти док в SiYuan ТОЧНО по hpath (заголовки не уникальны между проектами)
        doc = sy.get_doc_id_by_path(nb, hpath)
        if not doc:
            print("док не найден в SiYuan:", hpath[:60])
            notfound += 1
            continue
        hpath_real = hpath
        rows = [{"id": doc, "hpath": hpath}]
        md = ""
        try:
            r = sy._post("/api/export/exportMdContent", {"id": doc})
            md = (r or {}).get("content", "")
        except Exception:
            pass
        if not md:
            print("пустой экспорт:", title[:40]); notfound += 1; continue
        changed = False
        for it in items:
            bid, html = it["blockId"], it["html"]
            # афина экспортировала комментарий с &#45;-эскейпом и без
            for marker in (f"<!-- unsupported: flavour=affine:embed&#45;html blockId={bid} -->",
                           f"<!-- unsupported: flavour=affine:embed-html blockId={bid} -->"):
                if marker in md:
                    md = md.replace(marker, f"```html\n{html}\n```")
                    changed = True
                    break
        if not changed:
            # блока-комментария нет (док менялся после миграции) — допишем в конец
            for it in items:
                md += f"\n```html\n{it['html']}\n```\n"
                changed = True
        # пересоздать док с тем же hpath
        try:
            hpath_real = next(r["hpath"] for r in rows if r["id"] == doc)
            sy._post("/api/filetree/removeDocByID", {"id": doc})
            time.sleep(1.2)
            sy.create_doc_with_md(nb, hpath_real, md)
            fixed += 1
            print("восстановлен:", hpath_real[:70], f"({len(items)} диагр.)")
        except Exception as e:
            print("FAIL:", title[:40], str(e)[:80])
            skipped += 1
        time.sleep(0.5)
    print(f"DONE fixed={fixed} skipped={skipped} notfound={notfound}")


if __name__ == "__main__":
    main()
