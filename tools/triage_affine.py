#!/usr/bin/env python3
"""triage_affine — чистка мусорных фактов в AFFiNE дейли-бакетах.

Замыкает dreaming-контур в AFFiNE-мире: секции бакета → rank триаж-головой
(подсветка p_garbage) → ручной вердикт → пересборка бакета (replace_doc_with_markdown)
+ логирование в корпус facts.triage.json.

Требования: контент-кэш после sync (для sections), MCP-шлюз (для apply);
ранкинг — tools/.venv-triage (numpy) + ollama qwen3-embedding.

Команды:
  sections <date>                    — распарсить бакет дня из кэша, ранжировать секции
  apply <date> --drop s1,s2 [--yes]  — пересобрать бакет без указанных слагов
                                       (без --yes — dry-run: показать план);
                                       слаги удаляются из affine-мапы (факт сможет
                                       переиздаться при переизвлечении)
  corpus <date> --keep s1,s2         — залогировать keep-вердикты в корпус
  (apply логирует drop-секции в корпус как garbage автоматически)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

WIKI = Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))
CACHE = WIKI / "affine_content_cache.json"
TOOLS = Path(__file__).resolve().parent
VENV_PY = TOOLS / ".venv-triage/bin/python"
TMP = Path("/tmp/opencode/triage-affine")
MCP_URL = os.environ.get(
    "UWUWU_AFFINE_URL", "https://affine-mcp-production.up.railway.app/mcp"
)
MCP_TOKEN = os.environ.get("UWUWU_AFFINE_TOKEN", "")
WORKSPACE = os.environ.get("UWUWU_AFFINE_WORKSPACE_ID", "")
if not WORKSPACE:  # fish universal vars не видны из bash-подпроцессов
    for map_file in (Path.home() / ".uwuwu").glob("affine_map_*_*.json"):
        m = re.search(r"-([0-9a-f-]{36})\.json$", map_file.name)
        if m:
            WORKSPACE = m.group(1)
            break
MCP_HEADERS = {
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
}

SLUG_RE = re.compile(r"slug: ([a-z0-9-]+)")
SECTION_RE = re.compile(r"(?m)^## ")


def load_bucket_markdown(date: str) -> tuple[str, str]:
    """(markdown, doc_id) бакета из контент-кэша."""
    cache = json.loads(CACHE.read_text(encoding="utf-8"))
    docs = cache.get("docs", cache)  # кэш может быть вложен в "docs"
    doc = docs.get(date) or next(
        (v for v in docs.values() if isinstance(v, dict) and v.get("title") == date),
        None,
    )
    if not doc:
        sys.exit(f"бакет {date} не найден в контент-кэше (sync?)")
    return doc["markdown"], doc["doc_id"]


def parse_sections(markdown: str) -> list[dict]:
    """Секции '## title' → {title, body, slug}."""
    parts = SECTION_RE.split(markdown)
    sections = []
    for raw in parts[1:]:
        lines = raw.splitlines()
        title = lines[0].strip()
        body = "\n".join(lines[1:]).strip()
        m = SLUG_RE.search(raw)
        sections.append({"title": title, "body": body, "slug": m.group(1) if m else ""})
    return sections


def section_statement(sec: dict) -> str:
    """Тело секции без мета-строки — как statement факта (на нём училась голова)."""
    lines = [ln for ln in sec["body"].splitlines() if not ln.strip().startswith("*")]
    return "\n".join(lines).strip()


def write_section_files(date: str, sections: list[dict]) -> list[Path]:
    """Темп-файлы-факты для triage_train rank (нужен frontmatter + тело)."""
    d = TMP / date
    d.mkdir(parents=True, exist_ok=True)
    files = []
    for i, sec in enumerate(sections):
        f = d / f"{sec['slug'] or f'no-slug-{i}'}.md"
        f.write_text(
            f"---\nslug: {sec['slug']}\ndescription: {sec['title']}\n---\n"
            f"{section_statement(sec)}\n",
            encoding="utf-8",
        )
        files.append(f)
    return files


def cmd_sections(args: argparse.Namespace) -> int:
    markdown, _ = load_bucket_markdown(args.date)
    sections = parse_sections(markdown)
    if not sections:
        print(f"бакет {args.date}: секций нет")
        return 0
    files = write_section_files(args.date, sections)
    print(f"бакет {args.date}: {len(sections)} секций, ранжирую…\n", flush=True)
    return subprocess.call(
        [str(VENV_PY), str(TOOLS / "triage_train.py"), "rank", *(str(f) for f in files)]
    )


def mcp_call(
    method: str, params: dict, sid: str | None = None, notify: bool = False
) -> tuple[object, str]:
    headers = dict(MCP_HEADERS)
    if MCP_TOKEN:
        headers["Authorization"] = f"Bearer {MCP_TOKEN}"
    if sid:
        headers["mcp-session-id"] = sid
    payload: dict = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notify:
        payload["id"] = 1  # notification не имеет id (JSON-RPC 2.0)
    req = urllib.request.Request(
        MCP_URL, data=json.dumps(payload).encode(), headers=headers
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        sid = sid or r.headers.get("mcp-session-id")
        body = r.read().decode()
    if not body.strip():
        # 202 на notification — пустое тело валидно
        return {}, sid or ""
    if body.startswith("event:"):
        data = [ln[6:] for ln in body.splitlines() if ln.startswith("data: ")]
        body = data[-1] if data else "{}"
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError as e:
        print(
            f"!! MCP {method}: не-JSON ответ (status {r.status}, "
            f"{len(body)} байт): {body[:200]!r}",
            file=sys.stderr,
        )
        raise SystemExit(2) from e
    return parsed, sid or ""


def mcp_result(resp: object) -> dict:
    """Валидированный result: JSON-RPC error / isError / ok:false → exit ДО мутаций."""
    if not isinstance(resp, dict) or "error" in resp or "result" not in resp:
        print(f"!! шлюз вернул ошибку: {json.dumps(resp, ensure_ascii=False)[:300]}")
        sys.exit(2)
    result = resp["result"]
    if not isinstance(result, dict):
        return {}
    if result.get("isError") or result.get("ok") is False:
        detail = ""
        for c in result.get("content", []):
            if isinstance(c, dict) and c.get("text"):
                detail = c["text"]
                break
        print(
            f"!! инструмент отказал: {detail or json.dumps(result, ensure_ascii=False)[:300]}"
        )
        sys.exit(2)
    return result


def cmd_apply(args: argparse.Namespace) -> int:
    drop = {s.strip() for s in args.drop.split(",") if s.strip()}
    markdown, doc_id = load_bucket_markdown(args.date)
    sections = parse_sections(markdown)
    keep_secs = [s for s in sections if s["slug"] not in drop]
    dropped = [s for s in sections if s["slug"] in drop]
    missing = drop - {s["slug"] for s in sections}
    print(
        f"бакет {args.date} ({doc_id}): {len(sections)} секций, "
        f"drop {len(dropped)}, останется {len(keep_secs)}"
    )
    if missing:
        print(f"!! слаги не найдены в бакете: {sorted(missing)}")
    for s in dropped:
        print(f"  − {s['slug']} :: {s['title'][:70]}")

    if not args.yes:
        print("\n(dry-run: добавь --yes для применения)")
        return 0

    # 1) пересборка markdown: префикс дока + raw-секции в ИСХОДНОМ порядке,
    #    соответствие секция↔raw по позиции (split и parse шли по одним границам)
    raws = SECTION_RE.split(markdown)
    rebuilt = raws[0]
    for sec, raw in zip(sections, raws[1:]):
        if sec["slug"] not in drop:
            rebuilt += "## " + raw
    rebuilt = rebuilt.strip() + "\n"

    # 2) корпус-записи валидируем ДО replace (пустой/короткий statement не
    #    должен ронять логирование ПОСЛЕ мутации дока)
    sys.path.insert(0, str(TOOLS))
    from triage_log import add, load, save

    store = load()
    corpus_logged = 0
    for s in dropped:
        stmt = section_statement(s) or s["title"]
        try:
            add(store, stmt, "garbage", args.date, f"affine/{args.date}/{s['slug']}")
            corpus_logged += 1
        except SystemExit:
            print(f"  warn: секция {s['slug']} не влезла в корпус (короткий текст)")

    # 3) MCP: одна сессия — replace контента; ошибка = exit до мутаций
    resp, sid = mcp_call(
        "initialize",
        {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "triage-affine", "version": "1"},
        },
    )
    mcp_result(resp)
    mcp_call("notifications/initialized", {}, sid, notify=True)
    resp, _ = mcp_call(
        "tools/call",
        {
            "name": "replace_doc_with_markdown",
            "arguments": {
                "workspaceId": WORKSPACE,
                "docId": doc_id,
                "markdown": rebuilt,
            },
        },
        sid,
    )
    mcp_result(resp)

    # 4) мапа: убрать дропнутые слаги (факт сможет переиздаться);
    #    при нескольких мапах берём самую свежую по mtime
    candidates = [WIKI / "affine_map.json", *WIKI.glob("affine_map_*.json")]
    map_path = max(
        (p for p in candidates if p.exists()), key=lambda p: p.stat().st_mtime
    )
    m = json.loads(map_path.read_text(encoding="utf-8"))
    removed_map = [s for s in drop if m.pop(s, None) is not None]
    map_path.write_text(json.dumps(m, ensure_ascii=False, indent=1), encoding="utf-8")
    save(store)
    print(
        f"готово: бакет пересобран ({len(keep_secs)} секций), мапа −{len(removed_map)}, "
        f"корпус +{corpus_logged} garbage ({map_path.name})"
    )
    print("напоминание: uwuwu-cli sync — обновить кэши после правки")
    return 0


def log_corpus(sections: list[dict], label: str, day: str) -> None:
    sys.path.insert(0, str(TOOLS))
    from triage_log import add, load, save

    store = load()
    for s in sections:
        add(
            store,
            section_statement(s) or s["title"],
            label,
            day,
            f"affine/{day}/{s['slug']}",
        )
    save(store)


def cmd_corpus(args: argparse.Namespace) -> int:
    keep = {s.strip() for s in args.keep.split(",") if s.strip()}
    markdown, _ = load_bucket_markdown(args.date)
    sections = [s for s in parse_sections(markdown) if s["slug"] in keep]
    found = {s["slug"] for s in sections}
    missing = keep - found
    if missing:
        print(f"!! слаги не найдены в бакете: {sorted(missing)}")
    log_corpus(sections, "keep", args.date)
    print(f"корпус +{len(sections)} keep ({args.date})")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    s1 = sub.add_parser("sections", help="ранжировать секции бакета")
    s1.add_argument("date", help="YYYY-MM-DD")
    a = sub.add_parser("apply", help="пересобрать бакет без drop-слагов")
    a.add_argument("date")
    a.add_argument("--drop", required=True, help="slug1,slug2,…")
    a.add_argument("--yes", action="store_true", help="применить (иначе dry-run)")
    c = sub.add_parser("corpus", help="залогировать keep-вердикты")
    c.add_argument("date")
    c.add_argument("--keep", required=True, help="slug1,slug2,…")
    args = p.parse_args()
    return {"sections": cmd_sections, "apply": cmd_apply, "corpus": cmd_corpus}[
        args.cmd
    ](args)


if __name__ == "__main__":
    sys.exit(main())
