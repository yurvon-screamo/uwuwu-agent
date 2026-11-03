"""Stage 2: transform exported markdown for SiYuan import.

- ```html_embed name fences → raw HTML inline (SiYuan renders HTML blocks)
- affine://blob/<key> refs → relative assets/<key> refs (uploaded at import)
- drops affine-specific noise if any
"""
from __future__ import annotations

import re
from pathlib import Path

EMBED_FENCE_RE = re.compile(
    r"```html_embed[^\n]*\n(.*?)\n```", re.DOTALL
)
BLOB_REF_RE = re.compile(r"affine://blob/([A-Za-z0-9_.\-]+)")


def transform_markdown(md: str) -> tuple[str, int, int]:
    """Returns (new_md, unwrapped_fences, blob_refs)."""
    fences = 0

    def _unwrap(m: re.Match) -> str:
        nonlocal fences
        fences += 1
        return m.group(1)

    md = EMBED_FENCE_RE.sub(_unwrap, md)
    blob_keys = sorted(set(BLOB_REF_RE.findall(md)))
    md = BLOB_REF_RE.sub(lambda m: f"assets/{m.group(1)}", md)
    return md, fences, len(blob_keys)


def transform_tree(export_dir: Path, out_dir: Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    stats = {"files": 0, "fences": 0, "blobs": 0}
    for f in sorted(export_dir.rglob("*.md")):
        rel = f.relative_to(export_dir)
        md = f.read_text(encoding="utf-8")
        md2, fences, blobs = transform_markdown(md)
        dest = out_dir / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(md2, encoding="utf-8")
        stats["files"] += 1
        stats["fences"] += fences
        stats["blobs"] += blobs
    return stats


if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=str(Path.home() / "siyuan-migration-work" / "export"))
    ap.add_argument("--dst", default=str(Path.home() / "siyuan-migration-work" / "transformed"))
    a = ap.parse_args()
    print(transform_tree(Path(a.src), Path(a.dst)))
