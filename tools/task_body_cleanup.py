#!/usr/bin/env python3
"""task_body_cleanup — драйвер среза C: чистка offender'ов тел задач.

Гварды на каждое тело (по матрице плана, канон — factory/TASK-TEMPLATE.md):
  1. freshness: sha256 свежего export == снимку аудита; расходится → скип
     «пересканировать», мутации нет (паттерн freshness-гварда wiki_ops PR #98)
  2. --backup тела в data/assets/undo/task-bodies/ + копия в scratch
  3. трансформ контента; секция «## Комменты» переносится дословно
  4. task body --replace (cli-путь: docId стабилен, привязка строки жива)
  5. пост-чек: export не пуст; «## Комменты» нового экспорта == бэкапу по
     нормализованной сверке (collapse-whitespace + вырез zero-width +
     схлоп артефактных #-цепочек — kernel round-trip недетерминирован на
     пробелах/ZWSP, проверено на задаче 20261106071828); байтовый diff
     секции до/после — артефакт в scratch

Механический режим (--fix-mechanical) — только безопасные трансформы:
  - срез content-H1-блоков, равных тайтлу дока (дубль-H1 — баг старого костяка)
  - схлоп подряд идущих пустых дублей H2-секции (осиротевшие заглушки replace)
Ручной режим (--apply) — пересобранное агентом тело из файла под тем же
гвардом; никакой автоматики над смыслом.

Использование:
  task_body_cleanup.py --fix-mechanical audit.json --scratch D
  task_body_cleanup.py --apply <docId> --project <proj> --md <файл> \
      --sha <sha-из-аудита> [--board-doc <docId>] --scratch D
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess  # nosec B404 — argv списком, бинарь фиксирован shutil.which
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wiki_ops  # noqa: E402
import task_body_audit as tba  # noqa: E402 — strip/classify/constants

UWUWU_CLI = tba.UWUWU_CLI
DEFAULT_SCRATCH = "./task-bodies-cleanup"
ZW = "\u200b\ufeff\u2060"


def sha16(text: str) -> str:
    """Свежесть — по НОРМАЛИЗОВАННОЙ форме контента без frontmatter (lastmod
    ядро двигает при каждом реимпорте; рендер-строка имени стабильна).
    Нормализованная форма стабильна при round-trip и ловит любую
    содержательную правку."""
    content = tba.strip_frontmatter(text)
    return hashlib.sha256(
        "\n".join(wiki_ops._normalized_lines(content)).encode()).hexdigest()[:16]


def normalize(s: str) -> str:
    """Нормализованная форма для пост-чека «Комментов»: боевой нормализатор
    wiki_ops (пробельные разбеги, пайпы таблиц, separator-пэддинг, пустые
    строки — всё это ядро недетерминированно меняет при roundtrip) + вырез
    zero-width и схлоп артефактных #-цепочек (вставки при реимпорте,
    задача 20261106071828). Байтовая сверка невыполнима, критерий —
    «текст без потерь»."""
    s = s.translate({ord(c): None for c in ZW})
    s = re.sub(r"(#+)(\s#+)+", r"\1", s)
    return "\n".join(wiki_ops._normalized_lines(s))


def comments_section(export_md: str) -> str:
    i = export_md.find("\n## Комменты\n")
    return export_md[i + 1:] if i != -1 else ""


def fresh(doc_id: str) -> str:
    return wiki_ops._export_content(doc_id)


def guard_freshness(doc_id: str, sha_snapshot: str) -> str | None:
    export_md = fresh(doc_id)
    if not export_md.strip():
        return None
    if sha16(export_md) != sha_snapshot:
        print(f"SKIP {doc_id}: тело изменилось после аудита (sha) — пересканировать")
        return None
    return export_md


def backup(doc_id: str, export_md: str, scratch: Path) -> str:
    now = int(time.time())
    entry = {"ts": now, "op": "task-body-cleanup", "id": doc_id, "before_md": export_md}
    name = f"{now}-{doc_id}.json"
    wiki_ops._put_workspace_file(
        f"data/assets/undo/task-bodies/{name}",
        json.dumps(entry, ensure_ascii=False, indent=1).encode())
    scratch.mkdir(parents=True, exist_ok=True)
    (scratch / f"{doc_id}.before.md").write_text(export_md, encoding="utf-8")
    return name


def _collapse_consecutive_dup_headers(content: str) -> str:
    """Подряд идущие заголовки одной секции (пустой дубль, осиротевший после
    чьего-то replace) → один. Небезопасное слияние непустых дублей — не здесь:
    контент между заголовками прерывает цепочку, такие дубли — ручной режим."""
    out, prev_header = [], None
    for line in content.split("\n"):
        m = re.match(r"^## (.+?)\s*$", line)
        if m:
            header = m.group(1)
            if prev_header == header:
                continue
            prev_header = header
        elif line.strip():
            prev_header = None
        out.append(line)
    return "\n".join(out)


def transform_mechanical(export_md: str, title: str) -> str:
    """Безопасные трансформы: срез content-H1 == тайтл; схлоп подряд пустых
    дублей H2. Смысл текста не трогается. «## Комменты» вырезаются здесь:
    досыпает их дословно apply_body — иначе задвоение секции."""
    content_full = tba.content_of(export_md, title)
    content, _ = tba.cut_comments(content_full)
    content = "\n".join(
        line for line in content.split("\n")
        if not re.fullmatch(rf"# {re.escape(title)}", line.strip()))
    content = _collapse_consecutive_dup_headers(content)
    return content.rstrip() + "\n"


def apply_body(doc_id: str, project: str, new_content: str,
               export_before: str, scratch: Path, label: str) -> bool:
    """Сборка нового тела (контент + «Комменты» дословно) → cli replace →
    пост-чек. Возвращает успех."""
    payload = new_content.rstrip() + "\n\n" + comments_section(export_before).lstrip("\n")
    md_file = scratch / f"{doc_id}.{label}.md"
    md_file.write_text(payload, encoding="utf-8")
    subprocess.run(  # nosec B603 — фиксированный бинарь, аргументы из аудита
        [UWUWU_CLI, "task", "body", doc_id, "--project", project, "--replace",
         "--md", str(md_file)],
        capture_output=True, text=True, check=True)
    after = fresh(doc_id)
    if not after.strip():
        print(f"FAIL {doc_id}: export пуст после replace — откат из undo-журнала")
        return False
    before_c, after_c = comments_section(export_before), comments_section(after)
    if normalize(before_c) != normalize(after_c):
        print(f"FAIL {doc_id}: «## Комменты» потеряны/изменены — откат из undo-журнала; "
              f"diff: {scratch / f'{doc_id}.comments.diff'}")
        (scratch / f"{doc_id}.comments.diff").write_text(
            f"--- ДО\n{before_c}\n--- ПОСЛЕ\n{after_c}", encoding="utf-8")
        return False
    (scratch / f"{doc_id}.after.md").write_text(after, encoding="utf-8")
    print(f"OK {doc_id}: {label} (замечаний нет; diff до/после в scratch)")
    return True


def _fix_one(e: dict, scratch: Path) -> str:
    """Механический прогон одного тела → "ok" | "skip" | "fail"."""
    if e["html"]:
        print(f"SKIP {e['doc_id']}: HTML/эмбед-блоки — только блочная хирургия, вручную")
        return "skip"
    try:
        export_before = guard_freshness(e["doc_id"], e["sha"])
        if export_before is None:
            return "skip"
        backup(e["doc_id"], export_before, scratch)
        new_content = transform_mechanical(export_before, e["title"])
        content_before, _ = tba.cut_comments(tba.content_of(export_before, e["title"]))
        if normalize(new_content) == normalize(content_before):
            print(f"SKIP {e['doc_id']}: механике нечего менять")
            return "skip"
        return "ok" if apply_body(e["doc_id"], e["board"], new_content,
                                  export_before, scratch, "mech") else "fail"
    except subprocess.CalledProcessError as exc:
        print(f"FAIL {e['doc_id']}: cli отказал — "
              f"{(exc.stderr or exc.stdout or '?').strip()[:160]}")
        return "fail"


def fix_mechanical(audit_path: str, scratch_dir: str, only_verdict: str | None,
                   only_doc: str | None) -> None:
    d = json.loads(Path(audit_path).read_text(encoding="utf-8"))
    scratch = Path(scratch_dir)
    tally = {"ok": 0, "skip": 0, "fail": 0}
    failures = []
    for e in d["cleanable"]:
        if only_verdict and e["verdict"] != only_verdict:
            continue
        if only_doc and e["doc_id"] != only_doc:
            continue
        outcome = _fix_one(e, scratch)
        tally[outcome] += 1
        if outcome == "fail":
            failures.append(e["doc_id"])
    print(f"\nитог mechanical: {tally}")
    if failures:
        print("необработанные:", ", ".join(failures))


def apply(audit_path: str, doc_id: str, project: str, md_file: str, scratch_dir: str) -> None:
    d = json.loads(Path(audit_path).read_text(encoding="utf-8"))
    entry = next((e for e in d["all"] if e["doc_id"] == doc_id), None)
    if entry is None:
        sys.exit(f"{doc_id} нет в снимке аудита")
    export_before = guard_freshness(doc_id, entry["sha"])
    if export_before is None:
        sys.exit(1)
    backup(doc_id, export_before, Path(scratch_dir))
    new_content = Path(md_file).read_text(encoding="utf-8")
    # ручной контент приходит без «Комментов» — драйвер дословно досыпает их
    if "## Комменты" in new_content:
        sys.exit("в пересобранном файле не должно быть секции «## Комменты» — её досыпает драйвер")
    if not apply_body(doc_id, project, new_content, export_before,
                      Path(scratch_dir), "hand"):
        sys.exit(1)


def fs_read(path: str) -> bytes:
    """Контент файла воркспейса через kernel getFile (в отличие от
    wiki_ops._fs_file, который отвечает статусом существования). Конверт
    code≠0 (файла нет) → пустые байты."""
    import urllib.request
    wiki_ops._conf()
    req = urllib.request.Request(  # nosec B310 — URL из _conf kernel-транспорта (scheme-guard)
        wiki_ops.URL + "/api/file/getFile",
        data=json.dumps({"path": path}).encode(),
        headers={"Authorization": f"Token {wiki_ops.TOKEN}",
                 "Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=30) as r:  # nosec B310 — URL из _conf (scheme-guard), путь — константа kernel API
        body = r.read()
    try:
        envelope = json.loads(body.decode())
    except ValueError:
        return body  # не JSON — реальный контент (markdown)
    return b"" if isinstance(envelope, dict) and "code" in envelope else body


def _journal_entry(doc_id: str, journal_ts: int | None) -> tuple[int, dict] | None:
    """Запись undo-журнала для дока: перебор timestamps вокруг целевого
    (журнал пишется с именем <unixts>-<docId>.json, точное ts заранее
    неизвестно). None — не найдена."""
    base = journal_ts if journal_ts is not None else int(time.time())
    candidates = [journal_ts] if journal_ts is not None else range(base - 3600, base + 60)
    for ts in candidates:
        raw = fs_read(f"data/assets/undo/task-bodies/{ts}-{doc_id}.json")
        if not raw:
            continue
        try:
            d = json.loads(raw.decode())
        except ValueError:
            continue
        if isinstance(d, dict) and "before_md" in d:
            return ts, d
    return None


def restore(doc_id: str, project: str, scratch_dir: str, journal_ts: int | None) -> None:
    """Откат тела из undo-журнала: before_md — полный экспорт, payload для
    replace должен быть без frontmatter и рендер-строки (иначе они ложат
    в контент блоками — восстановление само бы испортило тело)."""
    found = _journal_entry(doc_id, journal_ts)
    if not found:
        sys.exit(f"undo-журнал для {doc_id} не найден")
    ts, entry = found
    m = re.search(r"^title: (.+)$", entry["before_md"], flags=re.M)
    title = m.group(1).strip() if m else ""
    payload = tba.content_of(entry["before_md"], title)
    md = Path(scratch_dir) / f"{doc_id}.restore.md"
    md.parent.mkdir(parents=True, exist_ok=True)
    md.write_text(payload, encoding="utf-8")
    r = subprocess.run(  # nosec B603 — фиксированный бинарь
        [UWUWU_CLI, "task", "body", doc_id, "--project", project, "--replace",
         "--md", str(md)], capture_output=True, text=True, check=True)
    print(f"восстановлено из журнала {ts}: {(r.stdout or r.stderr).strip()[:120]}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    fm = sub.add_parser("fix-mechanical", help="механические трансформы по снимку аудита")
    fm.add_argument("audit", help="json-снимок task_body_audit")
    fm.add_argument("--scratch", default=DEFAULT_SCRATCH)
    fm.add_argument("--only-verdict", choices=["дубль-чата", "полуканон"])
    fm.add_argument("--doc", help="одно тело (проба механики на конкретном docId)")
    ap_apply = sub.add_parser("apply", help="ручное пересобранное тело под гвардами")
    ap_apply.add_argument("audit")
    ap_apply.add_argument("--doc", required=True)
    ap_apply.add_argument("--project", required=True)
    ap_apply.add_argument("--md", required=True)
    ap_apply.add_argument("--scratch", default=DEFAULT_SCRATCH)
    rs = sub.add_parser("restore", help="откат тела из undo-журнала task-bodies")
    rs.add_argument("--doc", required=True)
    rs.add_argument("--project", required=True)
    rs.add_argument("--scratch", default=DEFAULT_SCRATCH)
    rs.add_argument("--journal-ts", type=int, help="конкретная запись журнала (деф. первая найденная)")
    a = ap.parse_args()
    if a.cmd == "fix-mechanical":
        fix_mechanical(a.audit, a.scratch, a.only_verdict, a.doc)
    elif a.cmd == "apply":
        apply(a.audit, a.doc, a.project, a.md, a.scratch)
    else:
        restore(a.doc, a.project, a.scratch, a.journal_ts)


if __name__ == "__main__":
    main()
