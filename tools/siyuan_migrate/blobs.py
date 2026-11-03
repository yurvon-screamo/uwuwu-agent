"""Stage 1b: download AFFiNE blobs (images/attachments) referenced by exported docs.

Auth: UI sign-in (bot account) → cookie → GET /api/workspaces/{ws}/blobs/{key}.
"""
from __future__ import annotations

import argparse
import json
import os
import time
from http.cookiejar import CookieJar
from pathlib import Path
from urllib.request import Request, build_opener, HTTPCookieProcessor

WS = "81475705-028a-4153-a8f5-f06e402ee31e"


def make_opener(ui: str, email: str, password: str):
    jar = CookieJar()
    op = build_opener(HTTPCookieProcessor(jar))
    body = json.dumps({"email": email, "password": password}).encode()
    req = Request(ui.rstrip("/") + "/api/auth/sign-in", data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    with op.open(req, timeout=30) as resp:
        assert resp.status == 200, f"sign-in failed: {resp.status}"
    return op


def guess_ext(key: str, content_type: str) -> str:
    ct = content_type.split(";")[0].strip().lower()
    mapping = {
        "image/png": ".png", "image/jpeg": ".jpg", "image/gif": ".gif",
        "image/svg+xml": ".svg", "image/webp": ".webp",
        "application/pdf": ".pdf", "application/zip": ".zip",
        "text/markdown": ".md", "text/plain": ".txt",
    }
    if key and "." in key:
        return ""
    return mapping.get(ct, "")


def download_blobs(manifest_path: Path, out_dir: Path, ui: str, email: str, password: str) -> dict:
    manifest = json.loads(manifest_path.read_text())
    blobs = manifest.get("blobs", {})
    out_dir.mkdir(parents=True, exist_ok=True)
    op = make_opener(ui, email, password)
    stats = {"ok": 0, "skip": 0, "fail": 0}
    for i, key in enumerate(sorted(blobs)):
        dest = out_dir / key
        if dest.exists() and dest.stat().st_size > 0:
            stats["skip"] += 1
            continue
        url = f"{ui.rstrip('/')}/api/workspaces/{WS}/blobs/{key}"
        req = Request(url, method="GET")
        ok = False
        for attempt in range(3):
            try:
                with op.open(req, timeout=60) as resp:
                    data = resp.read()
                    ct = resp.headers.get("Content-Type", "")
                if len(data) < 16 and not ct.startswith("text"):
                    raise RuntimeError(f"suspiciously small blob ({len(data)}b)")
                ext = guess_ext(key, ct)
                final = dest if not ext else out_dir / (key + ext)
                final.write_bytes(data)
                # keep canonical key-name too if ext was appended
                if ext and not dest.exists():
                    dest.symlink_to(final.name)
                ok = True
                break
            except Exception as e:  # noqa: BLE001
                last = str(e)
                time.sleep(2 * (attempt + 1))
        if ok:
            stats["ok"] += 1
        else:
            print(f"blob FAIL {key}: {last}", flush=True)
            stats["fail"] += 1
        if i % 20 == 0:
            print(f"[{i+1}/{len(blobs)}] ok={stats['ok']} skip={stats['skip']} fail={stats['fail']}", flush=True)
    print("DONE", stats)
    return stats


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default=str(Path.home() / "siyuan-migration-work" / "export" / "manifest.json"))
    ap.add_argument("--out", default=str(Path.home() / "siyuan-migration-work" / "blobs"))
    ap.add_argument("--ui", default=os.environ.get("UWUWU_AFFINE_UI_URL", ""))
    ap.add_argument("--email", default=os.environ.get("UWUWU_AFFINE_EMAIL", ""))
    ap.add_argument("--password", default=os.environ.get("UWUWU_AFFINE_PASSWORD", ""))
    a = ap.parse_args()
    assert a.ui and a.email and a.password, "UI creds required"
    download_blobs(Path(a.manifest), Path(a.out), a.ui, a.email, a.password)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
