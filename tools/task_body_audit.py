#!/usr/bin/env python3
"""task_body_audit — инвентаризация и вердикты тел задач uwuwu (SiYuan).

Канон тел — factory/TASK-TEMPLATE.md (CI-сверка с cli body_template).
Скрипт — глаза чистки: вердикт каждому телу всех досок «Задачи <Proj>»,
список offender'ов для среза C. Read-only, ничего не мутирует.

Тип тела — по сигнатурным секциям (Варианты/Вывод/Границы → spike;
Сценарий (BDD)/Красный тест → bug; иначе dev), обязательные секции — своего
типа: тип-слепые критерии шили бы «полуканон» правильным spike/bug телам.
Секция «## Комменты» вырезается до классификации (легитимно разговорная).
Скелет — сверка с костяками-константами: новыми (сухой остаток dry-run не
нужен — шаблон без тайтла, заморожен здесь же) и легаси всех трёх типов
(с `# {title}`-блоком, до 06.11; dry-run легаси не отдаст — шаблон заменён;
источник заморозки — body_template коммита b0f903b).

Вердикты: канон / полуканон / дубль-чата / пустое / скелет.
Чистимые категории (решает срез C, не скрипт): дубль-чата и полуканон ×
Done/Draft/Todo. Кроны, живые статусы (In Progress/Design/Review/Needs
attention), пустой статус (легаси юзера), тела без строки — исключения.

Использование:
  task_body_audit.py [--board uwuwu] [--out файл.json]
      аудит всех досок (или одной) → сводка stdout + JSON-список тел
      (docId, тип, статус, вердикт, маркеры, sha256 экспорта — база
      freshness-свёрки чистки: sha снят в момент инвентаризации)
  task_body_audit.py --backup <docId> [--scratch-dir D]
      write-ahead бэкап тела: data/assets/undo/task-bodies/<ts>-<docId>.json
      (kernel putFile) + копия экспорта в D (деф. ./task-bodies-backup)
"""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import re
import shutil
import subprocess  # nosec B404 — argv списком, бинарь фиксирован shutil.which
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wiki_ops  # noqa: E402 — общий kernel-транспорт tools/

# имена канонических секций — едины для замороженных костяков и критериев
SETUP_SECTION = "Постановка"
CHECK_SECTION = "Проверка"
CONTEXT_SECTION = "Контекст"
OUTCOME_SECTION = "Вывод"
# вердикты
VERDICT_CANON, VERDICT_SEMI, VERDICT_CHAT = "канон", "полуканон", "дубль-чата"
VERDICT_EMPTY, VERDICT_SKELETON = "пустое", "скелет"

# ——— костяки: замороженные константы (клон collapse-сравнения cli guard'а) ———
# Новые (после канона 06.11): без H1, dev = Постановка/План/Проверка.
NEW_DEV = (f"## {SETUP_SECTION}\n\n## План\n<срезы/шаги, порядок и зависимости; в финале секция "
           f"перерастает в «Решение»>\n\n## {CHECK_SECTION}\n<исполняемое: команды, тесты, живой прогон>\n")
NEW_SPIKE = (f"## {CONTEXT_SECTION}\n\n## Варианты\n<что рассматривали: суть → плюсы/минусы/замеры; "
             f"сравнение — таблица>\n\n## {OUTCOME_SECTION}\n<рекомендация + доказательства: «путь A, "
             f"потому что …»>\n\n## Границы\n<что НЕ исследовано и почему; где вывод не действует>\n")
NEW_BUG = ("## Сценарий (BDD)\n```gherkin\nСценарий: <название>\n  Дано <состояние мира>\n"
           "  Когда <действие>\n  Тогда <ожидание>  # факт: <что было на самом деле>\n```\n\n"
           "## Красный тест\n<имя теста, который воспроизводит; не воспроизвёлся на стенде → "
           "детали окружения>\n\n## Причина\n<механизм: почему система так себя вела>\n\n"
           "## Фикс\n<что изменилось; красный тест теперь зелёный>\n\n## Профилактика\n"
           "<как класс бага закрыт: тест/инвариант/линтер>\n")
# Легаси (до 06.11): `# {title}` — реальный heading-блок в контенте; полный
# dev-набор Решение/Юзкейсы/Сиквенсы. {TITLE} подставляется при сравнении.
LEGACY_DEV = (f"# {{TITLE}}\n\n## {SETUP_SECTION}\n\n## Решение\n<принципиально, как для нового "
              "инженера: как теперь устроено и почему так — концепция, без файлов>\n\n"
              "## Юзкейсы\n<сценарии: кто что делает>\n\n## Сиквенсы\n<взаимодействия "
              f"текстом, по строке на шаг: А → Б: что>\n\n## {CHECK_SECTION}\n<исполняемое: команды, "
              "тесты, живой прогон>\n")
LEGACY_SPIKE = (f"# {{TITLE}}\n\n## {CONTEXT_SECTION}\n\n## Варианты\n<что рассматривали: суть → "
                f"плюсы/минусы/замеры; сравнение — таблица>\n\n## {OUTCOME_SECTION}\n<рекомендация + "
                f"доказательства: «путь A, потому что …»>\n\n## Границы\n<что НЕ исследовано "
                f"и почему; где вывод не действует>\n")
LEGACY_BUG = ("# {TITLE}\n\n## Сценарий (BDD)\n```gherkin\nСценарий: <название>\n  Дано <состояние мира>\n"
              "  Когда <действие>\n  Тогда <ожидание>  # факт: <что было на самом деле>\n```\n\n"
              "## Красный тест\n<имя теста, который воспроизводит; не воспроизвёлся на "
              "стенде → детали окружения>\n\n## Причина\n<механизм: почему система так себя "
              "вела>\n\n## Фикс\n<что изменилось; красный тест теперь зелёный>\n\n"
              "## Профилактика\n<как класс бага закрыт: тест/инвариант/линтер>\n")

SKELETONS = {  # новые (title-независимые) + легаси всех трёх типов ({TITLE}-зависимые)
    "dev": [NEW_DEV, LEGACY_DEV],
    "spike": [NEW_SPIKE, LEGACY_SPIKE],
    "bug": [NEW_BUG, LEGACY_BUG],
}

# ——— критерии по типу ———
REQUIRED = {
    "dev": [(SETUP_SECTION, CONTEXT_SECTION), (CHECK_SECTION,), ("План", "Решение")],
    "spike": [(CONTEXT_SECTION, SETUP_SECTION), (OUTCOME_SECTION,)],
    "bug": [("Сценарий (BDD)",), ("Причина",), ("Фикс",)],
}
LEGAL_EXTRA_H2 = {"Отчёт инженера", "Смежное", "Юзкейсы", "Сиквенсы", "Комменты"}

BOARD_PREFIX = "Задачи"
COMMENTS_H2 = "## Комменты"
RENDER_PAGE_SIZE = 50  # серверный pageSize ядра для database render
UWUWU_CLI = shutil.which("uwuwu-cli") or "uwuwu-cli"  # полный путь: bandit B607
_ID_RE = re.compile(r"^[\w\-/ .:%«»()]+$")
SQL_PAGE = "LIMIT {limit} OFFSET {offset}"

CHAT_MARKERS = [
    r"(?<![\w-])[Яя]\s+(посмотрел|сделал|поправил|исправил|взял|проверил|добавил|убрал|нашёл|нашел)",
    r"\bвзялся\b", r"\bхм\b", r"\bщас\b", r"\bсейчас\s+(сделаю|поправлю|посмотрю|проверю)",
    r"\bдавайте\b", r"\bпонял,?\s", r"\bпринято\b", r"\bок,\s",
]
LOG_MARKERS = [
    r"^#{1,3} (Ход работы|Лог|Прогресс|Шаг \d|Итерация)", r"^\s*[-*] \[.\] (шаг|Шаг)",
    r"\bкоммит \w{7,}\b", r"^\d{2}:\d{2}\s", r"\bсессия(я|и)?\s+(потеряна|рестарт)\b",
]


def sql_pages(stmt_format: str) -> list[dict]:
    """SQL пагинацией: ядро без явного LIMIT отдаёт первые 64 строки."""
    rows, offset = [], 0
    while True:
        batch = wiki_ops._sql_rows(stmt_format.format(limit=64, offset=offset), attempts=3)
        rows.extend(batch)
        if len(batch) < 64:
            return rows
        offset += 64


def strip_frontmatter(md: str) -> str:
    if not md.startswith("---\n"):
        return md
    rest = md[4:]
    if "\n---" in rest:
        return rest.split("\n---", 1)[1].lstrip("\n")
    return md


def content_of(export_md: str, title: str) -> str:
    """Контент без экспортной обёртки: frontmatter + ОДНОЙ рендер-строки
    `# {title}` (клон strip_export_wrapper: легаси-H1-блок остаётся)."""
    body = strip_frontmatter(export_md).lstrip()
    prefix = f"# {title}"
    if body.startswith(prefix):
        return body[len(prefix):].lstrip("\n")
    return body


def h2_list(content: str) -> list[str]:
    fenced = False
    out = []
    for line in content.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if not fenced:
            m = re.match(r"^## (.+?)\s*$", line)
            if m:
                out.append(m.group(1).strip())
    return out


def cut_comments(content: str) -> tuple[str, bool]:
    m = re.search(rf"^{re.escape(COMMENTS_H2)}\s*$", content, flags=re.M)
    if not m:
        return content, False
    return content[: m.start()].rstrip() + "\n", True


def detect_type(h2s: list[str]) -> str:
    s = set(h2s)
    if "Красный тест" in s or "Сценарий (BDD)" in s:
        return "bug"
    if ({"Варианты", OUTCOME_SECTION} <= s) or ({OUTCOME_SECTION, "Границы"} <= s):
        return "spike"
    return "dev"


def collapse(s: str) -> str:
    return " ".join(s.split())


def is_skeleton(content_no_tag: str, title: str, ty: str) -> bool:
    stripped = "\n".join(line for line in content_no_tag.splitlines()
                         if line.strip() not in ("#dev", "#spike", "#bug"))
    norm = collapse(stripped)
    return any(norm == collapse(tpl.replace("{TITLE}", title))
               for tpl in SKELETONS.get(ty, []))


def _degradation_markers(content: str, h2s: list[str], h1_in_content: int) -> tuple[str, ...]:
    """Маркеры деградации: дубль-H1, разговорные вставки, логи хода, дубли секций."""
    markers = []
    if h1_in_content:
        markers.append(f"дубль-H1({h1_in_content})")
    chat = sum(len(re.findall(p, content, flags=re.I)) for p in CHAT_MARKERS)
    if chat:
        markers.append(f"разговорное({chat})")
    logs = sum(len(re.findall(p, content, flags=re.I | re.M)) for p in LOG_MARKERS)
    if logs:
        markers.append(f"лог-хода({logs})")
    dups = sorted({h for h in h2s if h2s.count(h) > 1})
    if dups:
        markers.append("дубль-секций:" + ",".join(dups))
    return tuple(markers)


CHAT_OR_LOG_PREFIXES = ("разговорное", "лог-хода", "дубль-секций")


def classify(export_md: str, title: str) -> dict:
    content_full = content_of(export_md, title)
    content, has_comments = cut_comments(content_full)
    h1_in_content = len(re.findall(r"^# ", content, flags=re.M))
    html = bool(re.search(r"```html|<iframe|⁂ diagram:|html_embed", content))
    if len(collapse(content)) < 30:
        return {"verdict": VERDICT_EMPTY, "type": detect_type(h2_list(content)),
                "markers": ["тело пустое/почти пустое"],
                "comments": has_comments, "html": html}
    h2s = h2_list(content)
    ty = detect_type(h2s)
    markers = list(_degradation_markers(content, h2s, h1_in_content))
    if is_skeleton(content, title, ty):
        return {"verdict": VERDICT_SKELETON, "type": ty, "markers": markers or ["нетронутый костяк"],
                "comments": has_comments, "html": html}
    missing = [alt[0] for alt in REQUIRED[ty] if not any(a in h2s for a in alt)]
    if missing:
        markers.append("нет " + "/".join(missing))
    if any(m.startswith(CHAT_OR_LOG_PREFIXES) for m in markers):
        verdict = VERDICT_CHAT
    elif missing:
        verdict = VERDICT_SEMI
    else:
        verdict = VERDICT_CANON
    return {"verdict": verdict, "type": ty, "markers": markers or ["ок"],
            "comments": has_comments, "html": html}


def sql_safe(value: str, what: str) -> str:
    """Гард интерполяции: в SQL попадают только значения ядра (id/hpath),
    валидированные по строгому паттерну — инъекция исключена."""
    if not _ID_RE.fullmatch(value):
        raise ValueError(f"подозрительное значение {what!r} для SQL-интерполяции")
    return value


def boards(only: str | None) -> list[dict]:
    rows = sql_pages(
        f"SELECT id, hpath, box FROM blocks WHERE type='d' AND hpath LIKE '/%{BOARD_PREFIX}%' "  # nosec B608 — интерполируется только константа BOARD_PREFIX
        f"ORDER BY hpath {SQL_PAGE}")
    out = []
    for r in rows:
        parts = r["hpath"].strip("/").split("/")
        if len(parts) == 2 and parts[1].startswith(BOARD_PREFIX) and (not only or parts[0] == only):
            out.append({"doc_id": r["id"], "hpath": r["hpath"], "project": parts[0]})
    return out


def board_rows(board: dict) -> list[dict]:
    """Строки доски: av-блок дока → `uwuwu-cli kernel render` (MCP-транспорт
    cli; сырой kernel-эндпоинт отдаёт определение вью без строк). Пагинация
    по rowCount с дедупом id — контракт cli read_rows."""
    av_rows = wiki_ops._sql_rows(
        f"select id, markdown from blocks where root_id='{sql_safe(board['doc_id'], 'board doc id')}' "  # nosec B608 — doc_id провалидирован sql_safe
        "and (type='av' or (type='i' and markdown like '%avID=%')) limit 5", attempts=3)
    av_id = block_id = ""
    for r in av_rows:
        m = re.search(r'(?:data-av-id|avID)=["\']([^"\']+)', r.get("markdown", ""))
        if m:
            av_id, block_id = m.group(1), r["id"]
            break
    if not av_id:
        return []
    return _render_rows(av_id, block_id)


def _render_page(av_id: str, block_id: str, page: int) -> dict:
    raw = subprocess.run(  # nosec B603 — бинарь фиксирован which, аргументы — id ядра
        [UWUWU_CLI, "kernel", "render", av_id, block_id, "--page", str(page)],
        capture_output=True, text=True, check=True).stdout
    return (json.loads(raw).get("data") or {}).get("view") or {}


def _fresh_rows(batch: list[dict], seen: set) -> list[dict]:
    fresh = []
    for it in batch:
        if not it.get("id") or it["id"] in seen:
            continue
        seen.add(it["id"])
        fresh.append(it)
    return fresh


def _render_items(av_id: str, block_id: str) -> tuple[list[dict], list[dict]]:
    """Все строки render: пагинация по rowCount (серверный pageSize — 50),
    дедуп id (дрейф страниц при конкурентных правках доски)."""
    seen, items, columns = set(), [], []
    for page in itertools.count(1):
        view = _render_page(av_id, block_id, page)
        if page == 1:
            columns = view.get("columns") or []
        batch = view.get("rows") or []
        if not batch:
            return items, columns
        items.extend(_fresh_rows(batch, seen))
        row_count = view.get("rowCount")
        reached = bool(row_count) and len(items) >= row_count
        if reached or len(batch) < RENDER_PAGE_SIZE:
            return items, columns


def _render_rows(av_id: str, block_id: str) -> list[dict]:
    """renderAttributeView через cli-пробу kernel render (MCP-транспорт cli;
    сырой kernel-эндпоинт отдаёт определение вью без строк)."""
    items, columns = _render_items(av_id, block_id)
    key_name = {k.get("id"): k.get("name", "") for k in columns}
    out = []
    for it in items:
        row = _row_cells(it, key_name)
        if row["title"]:
            out.append(row)
    return out


TITLE_KEYS = ("Primary Key", "Title", "Название", "Первичный ключ")
STATUS_KEYS = ("Status", "Статус")


def _row_cells(item: dict, key_name: dict) -> dict:
    row = {"title": "", "status": "", "body_doc": ""}
    for cell in item.get("cells") or []:
        name = key_name.get((cell.get("value") or {}).get("keyID"), "")
        val = cell.get("value") or {}
        if name in TITLE_KEYS:
            row["title"] = (val.get("block") or {}).get("content", "") or val.get("text", {}).get("content", "")
            row["body_doc"] = (val.get("block") or {}).get("id", "")
        elif name in STATUS_KEYS:
            opts = val.get("mSelect") or []
            row["status"] = ",".join(o.get("content", "") for o in opts)
    return row


def _body_entry(board: dict, body: dict, rows_by_doc: dict) -> dict:
    export_md = wiki_ops._export_content(body["id"])
    title = body.get("content", "")
    if not export_md.strip():
        return {"board": board["project"], "doc_id": body["id"], "title": title,
                "status": "(export пуст)", "verdict": VERDICT_EMPTY, "type": "?",
                "markers": ["export пуст"], "comments": False, "html": False,
                "sha": "", "row": ""}
    # статус — ТОЛЬКО по живой привязке строки: тайтл-фоллбек дал бы сироте
    # (строка снесена) чужой статус, и чистка пошла бы в тело мимо cli-резолва
    row = rows_by_doc.get(body["id"])
    cls = classify(export_md, title)
    return {"board": board["project"], "doc_id": body["id"], "title": title,
            "status": row["status"] if row else "(без строки)",
            "row": row["body_doc"] if row else "",
            "sha": hashlib.sha256(  # normalized-sha без frontmatter (lastmod
                # ядро двигает при каждом реимпорте); тот же регион хэша,
                # что sha16 драйвера чистки — freshness-сверка сходится
                "\n".join(wiki_ops._normalized_lines(
                    strip_frontmatter(export_md))).encode()).hexdigest()[:16],
            **cls}


def _audit_board(board: dict) -> list[dict]:
    rows = board_rows(board)
    bodies = sql_pages(
        f"SELECT id, content FROM blocks WHERE type='d' AND hpath LIKE "  # nosec B608 — hpath провалидирован sql_safe
        f"'{sql_safe(board['hpath'], 'board hpath')}/%' ORDER BY hpath {SQL_PAGE}")
    rows_by_doc = {r["body_doc"]: r for r in rows if r["body_doc"]}
    entries = [_body_entry(board, b, rows_by_doc) for b in bodies]
    body_ids = {b["id"] for b in bodies}
    entries += [
        {"board": board["project"], "doc_id": r["body_doc"], "title": r["title"],
         "status": r["status"], "row": r["body_doc"], "verdict": "(тело не найдено экспортом)",
         "type": "?", "markers": ["строка есть, тела нет"], "comments": False,
         "html": False, "sha": ""}
        for r in rows if r["body_doc"] and r["body_doc"] not in body_ids]
    return entries


LIVE_STATUSES = {"In Progress", "Design", "Review", "Needs attention"}
CLEANABLE_STATUSES = {"Done", "Draft", "Todo"}


def audit(only: str | None, out_path: str | None) -> None:
    all_boards = boards(only)
    result = [e for board in all_boards for e in _audit_board(board)]
    stats = {}
    for e in result:
        stats[e["verdict"]] = stats.get(e["verdict"], 0) + 1
    cleanable = [e for e in result if e["verdict"] in (VERDICT_CHAT, VERDICT_SEMI)
                 and e["status"] in CLEANABLE_STATUSES]
    excluded_live = [e for e in result if e["status"] in LIVE_STATUSES]
    excluded_empty = [e for e in result if e["status"] in ("", "(без строки)")
                      and e["verdict"] != VERDICT_EMPTY]
    payload = {"ts": int(time.time()), "boards": [b["hpath"] for b in all_boards],
               "stats": stats, "cleanable": cleanable, "excluded_live": excluded_live,
               "excluded_empty_status": excluded_empty, "all": result}
    text = json.dumps(payload, ensure_ascii=False, indent=1)
    if out_path:
        Path(out_path).write_text(text, encoding="utf-8")
    else:
        print(text)
    print(f"тел: {len(result)} | вердикты: {stats}")
    print(f"чистимые ({VERDICT_CHAT}|{VERDICT_SEMI} × Done/Draft/Todo): {len(cleanable)}")
    print(f"из них с HTML/эмбед-блоками (чистить только блочной хирургией): "
          f"{sum(1 for e in cleanable if e['html'])}")
    print(f"исключения: живые статусы {len(excluded_live)}, пустой статус {len(excluded_empty)}")


def backup(doc_id: str, scratch_dir: str) -> None:
    export_md = wiki_ops._export_content(doc_id)
    if not export_md.strip():
        sys.exit(f"export пуст для {doc_id} — бэкапить нечего, проверь id")
    now = int(time.time())
    entry = {"ts": now, "op": "task-body-backup", "id": doc_id, "before_md": export_md}
    wiki_ops._put_workspace_file(
        f"data/assets/undo/task-bodies/{now}-{doc_id}.json",
        json.dumps(entry, ensure_ascii=False, indent=1).encode())
    scratch = Path(scratch_dir)
    scratch.mkdir(parents=True, exist_ok=True)
    (scratch / f"{doc_id}.before.md").write_text(export_md, encoding="utf-8")
    print(f"бэкап: data/assets/undo/task-bodies/{now}-{doc_id}.json + {scratch}/{doc_id}.before.md")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", help="аудит одной доски (имя проекта/ноутбука)")
    ap.add_argument("--out", help="файл JSON-списка (деф. печать в stdout)")
    ap.add_argument("--backup", metavar="DOCID", help="write-ahead бэкап тела")
    ap.add_argument("--scratch-dir", default="./task-bodies-backup")
    a = ap.parse_args()
    if a.backup:
        backup(a.backup, a.scratch_dir)
        return
    audit(a.board, a.out)


if __name__ == "__main__":
    main()
