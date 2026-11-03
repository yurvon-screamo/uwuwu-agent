"""Stage 1: export AFFiNE workspace (organize tree + doc markdown + blob refs).

Resumable: skips docs already exported (marker in manifest.json).
Sequential on purpose — the MCP gateway is flaky under parallel load.
"""
from __future__ import annotations

import argparse
import json
import re
import socket
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

socket.setdefaulttimeout(90)  # зависший MCP-вызов не должен stall'ить экспорт

import wiki_ops  # noqa: E402

WS = "81475705-028a-4153-a8f5-f06e402ee31e"
BLOB_RE = re.compile(r"affine://blob/([A-Za-z0-9_.\-]+)")


def sanitize(name: str) -> str:
    name = name.strip().strip("/")
    return name or "untitled"


def build_tree(nodes: list[dict]) -> dict:
    by_id = {n["id"]: n for n in nodes}
    children: dict[str | None, list[dict]] = {}
    for n in nodes:
        children.setdefault(n.get("parentId"), []).append(n)
    for kids in children.values():
        kids.sort(key=lambda n: n.get("index", ""))
    return by_id, children


def walk(children: dict, parent: str | None, path: list[str], out: list[dict]):
    for n in children.get(parent, []):
        data = str(n.get("data") or "")
        p = path + [sanitize(data if n["type"] == "folder" else data)]
        if n["type"] == "doc":
            # для doc-нод: id = id строки дерева, data = НАСТОЯЩИЙ docId
            out.append({"id": n["id"], "doc_id": data, "path": p})
        walk(children, n["id"], p, out)


def export_doc(sid: str, doc_id: str, attempts: int = 3) -> tuple[str, str]:
    """Returns (title, markdown); retries on empty/flaky responses."""
    last_err = "unknown"
    for _ in range(attempts):
        r = wiki_ops.call(sid, "export_doc_markdown", {"workspaceId": WS, "docId": doc_id})
        payload = None
        for c in r.get("content", []):
            text = c.get("text") if isinstance(c, dict) else None
            if isinstance(text, str):
                try:
                    payload = json.loads(text)
                    break
                except json.JSONDecodeError:
                    continue
        if payload is None and isinstance(r.get("structuredContent"), dict):
            payload = r["structuredContent"]
        if payload is not None and payload.get("exists") is False:
            raise RuntimeError("doc missing (exists=false) — fast-fail, no retry")
        if payload and payload.get("exists"):
            md = payload.get("markdown") or ""
            title = payload.get("title") or doc_id
            if md.strip():
                return str(title), md
            last_err = "empty markdown"
        else:
            last_err = "missing payload"
        time.sleep(2)
    raise RuntimeError(f"export failed: {last_err}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(Path.home() / "siyuan-migration-work" / "export"))
    ap.add_argument("--limit", type=int, default=0, help="export at most N docs (0 = all)")
    ap.add_argument("--root", default="", help="only export under this top-level folder name")
    args = ap.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    manifest_path = out / "manifest.json"
    manifest = {"docs": {}, "blobs": {}, "errors": {}}
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())

    sid = wiki_ops.session("siyuan-migrate")
    r = wiki_ops.call(sid, "list_organize_nodes", {"workspaceId": WS})
    nodes = r.get("structuredContent", r).get("nodes", [])
    by_id, children = build_tree(nodes)
    docs: list[dict] = []
    walk(children, None, [], docs)
    if args.root:
        docs = [d for d in docs if d["path"] and d["path"][0] == args.root]
    total = len(docs)
    print(f"tree: {len(nodes)} nodes, {total} docs", flush=True)

    done = skipped = failed = 0
    t0 = time.time()
    for i, d in enumerate(docs):
        rel = "/".join(d["path"]) + ".md"
        if d["id"] in manifest["docs"] and manifest["docs"][d["id"]].get("ok"):
            skipped += 1
            continue
        if args.limit and done >= args.limit:
            break
        try:
            title, md = export_doc(sid, d["doc_id"])
            # последний сегмент пути = заголовок дока (tree даёт id, title — из экспорта)
            d["path"][-1] = sanitize(title)
            base = "/".join(d["path"])
            # развязка коллизий имён в одной папке
            rel = base + ".md"
            n = 2
            while (out / rel).exists() and manifest["docs"].get(d["id"], {}).get("path") != rel:
                rel = f"{base} ({n}).md"
                n += 1
            f = out / rel
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(md, encoding="utf-8")
            for key in BLOB_RE.findall(md):
                manifest["blobs"].setdefault(key, {"refs": 0})
                manifest["blobs"][key]["refs"] += 1
            manifest["docs"][d["id"]] = {"ok": True, "path": rel}
            done += 1
        except Exception as e:  # noqa: BLE001 — keep going, report at end
            manifest["errors"][d["id"]] = str(e)[:300]
            manifest["docs"][d["id"]] = {"ok": False, "path": rel}
            failed += 1
            time.sleep(3)
        if i % 5 == 0 or i == total - 1:
            manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=1))
            rate = (i + 1) / max(time.time() - t0, 1)
            eta = (total - i - 1) / max(rate, 0.01) / 60
            print(f"[{i+1}/{total}] done={done} skip={skipped} fail={failed} "
                  f"rate={rate:.2f}d/s eta={eta:.0f}m", flush=True)
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=1))
    print(f"FINISHED docs={done} skipped={skipped} failed={failed} blobs={len(manifest['blobs'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
