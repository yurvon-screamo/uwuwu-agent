"""Stage 3: import transformed markdown tree into SiYuan.

Top-level export folders → notebooks (Experience, Projects, Daily, ...).
Sub-folders → doc tree via createDocWithMd hPath.
Assets referenced by docs → workspace assets (upload once, dedup by key).

Resumable: skips docs that already exist at the target path.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from .siyuan_client import SiYuan


def ensure_path(sy: SiYuan, nb: str, parts: tuple[str, ...]) -> None:
    """createDocWithMd не создаёт промежуточные папки — строим по сегментам."""
    for i in range(1, len(parts) + 1):
        hpath = "/" + "/".join(parts[:i])
        if not sy.doc_exists(nb, hpath):
            if i < len(parts):
                sy.create_doc_with_md(nb, hpath, f"# {parts[i-1]}\n")
            # лист создаст вызывающий со своим контентом


def import_tree(src: Path, sy: SiYuan, dry_run: bool = False, limit: int = 0,
                asset_map: dict | None = None) -> dict:
    stats = {"docs": 0, "skipped": 0, "failed": 0, "notebooks": {}}

    def rewrite(md: str) -> str:
        if not asset_map:
            return md
        for key, dest in asset_map.items():
            if dest:
                md = md.replace(f"assets/{key}", dest)
        return md

    tops = sorted(p for p in src.iterdir() if p.is_dir())
    for top in tops:
        nb_name = top.name
        if dry_run:
            print(f"[dry] notebook: {nb_name}")
            stats["notebooks"][nb_name] = "dry"
            continue
        nb = sy.ensure_notebook(nb_name)
        stats["notebooks"][nb_name] = nb
        for f in sorted(top.rglob("*.md")):
            if limit and stats["docs"] >= limit:
                return stats
            rel = f.relative_to(top).with_suffix("")
            hpath = "/" + "/".join(rel.parts)
            if sy.doc_exists(nb, hpath):
                stats["skipped"] += 1
                continue
            md = rewrite(f.read_text(encoding="utf-8"))
            try:
                ensure_path(sy, nb, tuple(rel.parts))
                sy.create_doc_with_md(nb, hpath, md)
                stats["docs"] += 1
            except Exception as e:  # noqa: BLE001
                print(f"FAIL {hpath}: {e}", flush=True)
                stats["failed"] += 1
    return stats


def import_assets(manifest_blobs: dict, blob_dir: Path, sy: SiYuan) -> dict:
    """Upload downloaded blobs (named <key>) into SiYuan assets; map key→server path."""
    stats = {"uploaded": 0, "missing": 0, "map": {}}
    for key in manifest_blobs:
        f = blob_dir / key
        if not f.exists():
            stats["missing"] += 1
            continue
        try:
            dest = sy.upload_asset(f)
            stats["map"][key] = dest
            stats["uploaded"] += 1
        except Exception as e:  # noqa: BLE001
            print(f"asset FAIL {key}: {e}", flush=True)
            stats["missing"] += 1
    return stats


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=str(Path.home() / "siyuan-migration-work" / "transformed"))
    ap.add_argument("--ws", default=str(Path.home() / "siyuan-demo-ws"))
    ap.add_argument("--url", default="http://127.0.0.1:6806")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--assets", default="", help="dir with downloaded blobs; if set, upload assets only")
    a = ap.parse_args()

    sy = SiYuan.from_workspace_conf(Path(a.ws), a.url)
    print("siyuan version:", sy.version())

    if a.assets:
        mf = Path(Path(a.src).parent / "export" / "manifest.json")
        blobs = json.loads(mf.read_text())["blobs"] if mf.exists() else {}
        res = import_assets(blobs, Path(a.assets), sy)
        # сохранить карту ключ→путь для рерайта ссылок при импорте доков
        map_path = Path(a.src) / "_asset_map.json"
        map_path.write_text(json.dumps(res["map"], ensure_ascii=False, indent=1))
        print(res, "| map →", map_path)
        return 0

    amap = {}
    amap_path = Path(a.src) / "_asset_map.json"
    if amap_path.exists():
        amap = json.loads(amap_path.read_text())
        print(f"asset map: {len(amap)} entries")

    stats = import_tree(Path(a.src), sy, dry_run=a.dry_run, limit=a.limit, asset_map=amap)
    print(json.dumps(stats, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
