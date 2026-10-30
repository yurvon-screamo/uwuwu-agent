#!/usr/bin/env python3
"""triage_affine — чистка мусорных фактов в AFFiNE дейли-бакетах.

Замыкает dreaming-контур в AFFiNE-мире: секции бакета → ранжирование
ансамблем (logreg-голова + d3:2b decision-модель, bake-off 2026-10-24:
dual-agreement P 0.973 на корпусе 1078) → ручной вердикт → пересборка.

Требования: контент-кэш после sync (для sections), MCP-шлюз (для apply);
ранкинг — tools/.venv-triage (numpy) + ollama (qwen3-embedding, d3:2b).

Команды:
  sections <date>                    — таблица секций: p_logreg | p_d3,
                                       маркер A у dual-agreement-кандидатов
  apply <date> [--drop s1,s2] [--auto] [--yes]
                                     — пересобрать бакет; --drop — ручные
                                       слаги (навсегда: rm + мапа + корпус);
                                       --auto — dual-agreement-секции
                                       (lr≥0.6 ∧ d3≥0.18) В КАРАНТИН-док
                                       '<date> [quarantine]' (обратимо, мапу
                                       не трогаем — факт не переиздаётся);
                                       без --yes — dry-run
  corpus <date> --keep s1,s2         — залогировать keep-вердикты в корпус
  (ручные --drop логируются в корпус как garbage; авто-вердикты в корпус
   НЕ логируются — корпус только для ручных разметок, иначе переобучение
   сядет на само-подтверждение ансамбля)
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

# Ансамбль (bake-off 2026-10-24, корпус 1078: dual P 0.973 / покрытие 39%)
D3_MODEL = "d3:2b"
LR_AUTO = 0.6
D3_AUTO = 0.18


def load_bucket_markdown(date: str) -> tuple[str, str]:
    """(markdown, doc_id) бакета из контент-кэша.

    Fallback: кэш битый/отстал (sync-баг) — doc_id из affine-мапы
    (__daily_<date>, пишет publisher), markdown добывается живьём."""
    cache = json.loads(CACHE.read_text(encoding="utf-8"))
    docs = cache.get("docs", cache)  # кэш может быть вложен в "docs"
    doc = docs.get(date) or next(
        (v for v in docs.values() if isinstance(v, dict) and v.get("title") == date),
        None,
    )
    if doc:
        return doc["markdown"], doc["doc_id"]
    # fallback: daily-ключ из мапы; markdown пустой — live-ветка добудет
    for p in [WIKI / "affine_map.json", *WIKI.glob("affine_map_*.json")]:
        if p.exists():
            m = json.loads(p.read_text(encoding="utf-8"))
            did = m.get(f"__daily_{date}")
            if did:
                return "", did
    sys.exit(f"бакет {date} не найден ни в кэше, ни в мапе (__daily_{date})")


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


def score_logreg(files: list[Path]) -> dict[str, float]:
    """p_garbage от logreg-головы через rank (кеш эмбеддингов тёплый).

    Отказ ранка (ollama лежит, venv сломан) = fail-fast: тихая деградация
    «все скоринги -1 → авто находит 0 мусора» недопустима для авто-контура."""

    proc = subprocess.run(
        [
            str(VENV_PY),
            str(TOOLS / "triage_train.py"),
            "rank",
            *(str(f) for f in files),
        ],
        capture_output=True,
        text=True,
        timeout=1800,
        check=False,
    )
    if proc.returncode != 0:
        print(
            f"!! logreg-ранк упал (rc={proc.returncode}): {proc.stderr[-400:]}",
            file=sys.stderr,
        )
        sys.exit(2)
    scores = {}
    for line in proc.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 2:
            try:
                scores[Path(parts[1]).stem] = float(parts[0])
            except ValueError:
                continue
    if not scores:
        print(
            f"!! logreg-ранк не дал ни одного скоринга: {proc.stdout[-200:]}",
            file=sys.stderr,
        )
        sys.exit(2)
    return scores


def cmd_sections(args: argparse.Namespace) -> int:
    markdown, _ = load_bucket_markdown(args.date)
    if not markdown:  # кэш битый — берём живой док
        markdown, _ = live_bucket_markdown(args.date)
    sections = parse_sections(markdown)
    if not sections:
        print(f"бакет {args.date}: секций нет")
        return 0
    files = write_section_files(args.date, sections)
    print(f"бакет {args.date}: {len(sections)} секций; logreg…", flush=True)
    lr = score_logreg(files)
    print("d3:2b…", flush=True)
    from triage_bakeoff import systemone

    d3 = [systemone(D3_MODEL, section_statement(s) or s["title"]) for s in sections]
    auto_idx = {
        i
        for i, s in enumerate(sections)
        if lr.get(s["slug"], -1) >= LR_AUTO and d3[i] >= D3_AUTO
    }
    order = sorted(range(len(sections)), key=lambda i: -lr.get(sections[i]["slug"], 0))
    print(f"\n{'logreg':>7} {'d3:2b':>7}  авто  slug — заголовок")
    for i in order:
        s = sections[i]
        mark = "  A  " if i in auto_idx else "     "
        print(
            f"{lr.get(s['slug'], -1):>7.3f} {d3[i]:>7.3f} {mark}"
            f" {s['slug'] or '?'} — {s['title'][:52]}"
        )
    print(
        f"\nитого: {len(sections)}; dual-agreement (lr≥{LR_AUTO} ∧ d3≥{D3_AUTO}): "
        f"{len(auto_idx)} — кандидаты apply --auto (карантин, обратимо)"
    )
    return 0


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
    # SSE-стрим: сервер может слать ": keepalive"-комментарии ДО event/data
    # (замечено на больших ответах ~400KB — дерево воркспейса)
    data_lines = [ln[6:] for ln in body.splitlines() if ln.startswith("data: ")]
    if data_lines:
        body = data_lines[-1]
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


def get_md(o):  # noqa: S3776 — рекурсивный обход ответа, перенесён дословно из трёх байт-идентичных копий (behavior-preserving рефакторинг); упрощение рекурсии — отдельным сознательным шагом
    """Рекурсивно достать markdown из export-ответа (markdown-поле где-то
    в структуре: structuredContent или content[].text-JSON). Общий хелпер
    wiki_ops/wiki_audit/wiki_apply — правится в одном месте."""
    if isinstance(o, dict):
        for k, v in o.items():
            if k == "markdown" and isinstance(v, str):
                return v
            f = get_md(v)
            if f:
                return f
    elif isinstance(o, list):
        for i in o:
            f = get_md(i)
            if f:
                return f
    return None


def live_bucket_markdown(date: str) -> tuple[str, str]:
    """(markdown, doc_id) ПО ЖИВОМУ доку через MCP export — apply обязан
    работать с актуальным контентом: стейл-кэш вернул бы изъятые секции
    (lost-update против чужих правок между sync и apply)."""
    _, doc_id = load_bucket_markdown(date)  # doc_id из кэша (стабилен)
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
            "name": "export_doc_markdown",
            "arguments": {"workspaceId": WORKSPACE, "docId": doc_id},
        },
        sid,
    )
    res = mcp_result(resp)

    # конвенция шлюза: markdown либо в structuredContent, либо JSON-строкой
    # внутри content[0].text (см. affine-selfhost-mcp-integration)
    def extract_md(r: dict) -> str | None:
        sc = r.get("structuredContent")
        if isinstance(sc, dict) and isinstance(sc.get("markdown"), str):
            return sc["markdown"]
        for c in r.get("content", []):
            t = c.get("text") if isinstance(c, dict) else None
            if not t:
                continue
            try:
                inner = json.loads(t)
            except json.JSONDecodeError:
                if "## " in t:
                    return t
                continue
            if isinstance(inner, dict) and isinstance(inner.get("markdown"), str):
                return inner["markdown"]
        return None

    md = extract_md(res)
    if not md:
        print(f"!! export не вернул markdown: {json.dumps(res)[:300]}")
        sys.exit(2)
    return md, doc_id


def cmd_apply(args: argparse.Namespace) -> int:
    drop = (
        {s.strip() for s in args.drop.split(",") if s.strip()} if args.drop else set()
    )
    markdown, doc_id = live_bucket_markdown(args.date)
    sections = parse_sections(markdown)

    # dual-agreement-кандидаты (скорим всегда — dry-run тоже показывает план)
    auto_slugs: set[str] = set()
    if args.auto:
        files = write_section_files(args.date, sections)
        lr = score_logreg(files)
        from triage_bakeoff import systemone

        for i, sec in enumerate(sections):
            if sec["slug"] in drop:
                continue  # ручной вердикт приоритетнее
            p_d3 = systemone(D3_MODEL, section_statement(sec) or sec["title"])
            if lr.get(sec["slug"], -1) >= LR_AUTO and p_d3 >= D3_AUTO:
                auto_slugs.add(sec["slug"])

    manual = [s for s in sections if s["slug"] in drop]
    quarantined = [s for s in sections if s["slug"] in auto_slugs]
    keep_secs = [
        s for s in sections if s["slug"] not in drop and s["slug"] not in auto_slugs
    ]
    missing = drop - {s["slug"] for s in sections}
    print(
        f"бакет {args.date} ({doc_id}): {len(sections)} секций → "
        f"drop {len(manual)} (навсегда), карантин {len(quarantined)} (dual-agreement), "
        f"останется {len(keep_secs)}"
    )
    if missing:
        print(f"!! слаги не найдены в бакете: {sorted(missing)}")
    for s in manual:
        print(f"  − {s['slug']} :: {s['title'][:70]}")
    for s in quarantined:
        print(f"  ↦ {s['slug']} :: {s['title'][:70]}")

    if not args.yes:
        print("\n(dry-run: добавь --yes для применения)")
        return 0

    # 1) пересборка markdown: префикс дока + raw-секции в ИСХОДНОМ порядке,
    #    соответствие секция↔raw по позиции (split и parse шли по одним границам)
    removed = drop | auto_slugs
    raws = SECTION_RE.split(markdown)
    rebuilt = raws[0]
    for sec, raw in zip(sections, raws[1:]):
        if sec["slug"] not in removed:
            rebuilt += "## " + raw
    rebuilt = rebuilt.strip() + "\n"

    # 2) корпус-записи для РУЧНЫХ drop валидируем ДО мутаций (авто в корпус
    #    не пишем — только ручные разметки, иначе переобучение сядет на
    #    само-подтверждение ансамбля)
    sys.path.insert(0, str(TOOLS))
    from triage_log import add, load, save

    store = load()
    corpus_logged = 0
    for s in manual:
        stmt = section_statement(s) or s["title"]
        try:
            add(store, stmt, "garbage", args.date, f"affine/{args.date}/{s['slug']}")
            corpus_logged += 1
        except SystemExit:
            print(f"  warn: секция {s['slug']} не влезла в корпус (короткий текст)")

    # save корпуса ДО мутаций: add идемпотентен по sha(statement) — обрыв
    # после save оставит лейблы при живой секции (повтор перезапишет тем же),
    # вместо потери лейблов при удалённой секции
    save(store)

    # 3) MCP: одна сессия; порядок мутаций: карантин-append → daily-replace
    #    (обрыв между ними даст дубль секции в карантине — терпимо, карантин
    #    не претендует на уникальность; потеря невозможна: replace не идёт
    #    раньше успешного append)
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

    q_appended = 0
    if quarantined:
        q_doc = quarantine_doc(args.date, sid)
        q_md = ""
        for i, sec in enumerate(sections):
            if sec["slug"] in auto_slugs:
                q_md += "## " + raws[1 + i]
        resp, _ = mcp_call(
            "tools/call",
            {
                "name": "append_markdown",
                "arguments": {
                    "workspaceId": WORKSPACE,
                    "docId": q_doc,
                    "markdown": q_md,
                },
            },
            sid,
        )
        mcp_result(resp)
        q_appended = len(quarantined)

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

    # 4) мапа: убрать ТОЛЬКО ручные слаги (факт сможет переиздаться);
    #    карантин-слаги остаются в мапе — мусор не должен вернуться;
    #    при нескольких мапах берём самую свежую по mtime
    candidates = [WIKI / "affine_map.json", *WIKI.glob("affine_map_*.json")]
    map_path = max(
        (p for p in candidates if p.exists()), key=lambda p: p.stat().st_mtime
    )
    m = json.loads(map_path.read_text(encoding="utf-8"))
    removed_map = [s for s in drop if m.pop(s, None) is not None]
    map_path.write_text(json.dumps(m, ensure_ascii=False, indent=1), encoding="utf-8")
    print(
        f"готово: бакет пересобран ({len(keep_secs)} секций), карантин +{q_appended}, "
        f"мапа −{len(removed_map)} (ручные), корпус +{corpus_logged} garbage"
    )
    print("напоминание: uwuwu-cli sync — обновить кэши после правки")
    return 0


def quarantine_doc(date: str, sid: str) -> str:
    """doc_id карантин-дока '<date> [quarantine]'; создаётся при отсутствии.

    Ключ __quarantine_<date> в affine-мапе (service-префикс игнорируется
    Rust-кодом MigrationMap). Окно create→map-write микроскопично; обрыв в
    нём оставит осиротевший пустой карантин-док — повтор создаст второй
    (принятый риск: дубль пустого дока дешевле потери ключа)."""
    candidates = [WIKI / "affine_map.json", *WIKI.glob("affine_map_*.json")]
    map_path = max(
        (p for p in candidates if p.exists()), key=lambda p: p.stat().st_mtime
    )
    m = json.loads(map_path.read_text(encoding="utf-8"))
    key = f"__quarantine_{date}"
    if m.get(key):
        return m[key]
    title = f"{date} [quarantine]"
    header = (
        f"# {title}\n\nСекции, изъятые из дейли-бакета ансамблем триажа "
        f"(logreg ≥{LR_AUTO} ∧ {D3_MODEL} ≥{D3_AUTO}). Обратимо: вернуть "
        f"копированием секции обратно в бакет.\n"
    )
    resp, _ = mcp_call(
        "tools/call",
        {
            "name": "create_doc_from_markdown",
            "arguments": {"workspaceId": WORKSPACE, "title": title, "markdown": header},
        },
        sid,
    )
    res = mcp_result(resp)
    # строгий регекс по сериализованному ответу: порядок-зависимый обход dict
    # мог перехватить чужой "id" (job/run) вместо docId
    m_id = re.search(r'"(?:docId|doc_id)"\s*:\s*"([^"]+)"', json.dumps(res))
    doc_id = m_id.group(1) if m_id else None
    if not doc_id:
        print(f"!! не нашёл docId в ответе create: {json.dumps(res)[:300]}")
        sys.exit(2)
    m[key] = doc_id
    map_path.write_text(json.dumps(m, ensure_ascii=False, indent=1), encoding="utf-8")
    return doc_id


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
    a = sub.add_parser(
        "apply", help="пересобрать бакет (drop навсегда, auto — в карантин)"
    )
    a.add_argument("date")
    a.add_argument("--drop", help="slug1,slug2,… — ручные (навсегда)")
    a.add_argument(
        "--auto",
        action="store_true",
        help=f"dual-agreement lr≥{LR_AUTO} ∧ d3≥{D3_AUTO} — в карантин",
    )
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
