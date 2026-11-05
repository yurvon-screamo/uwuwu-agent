#!/usr/bin/env python3
"""wiki_ops — операции с доками uwuwu (SiYuan-бекенд). Единый CLI-модуль всех
wiki-операций агентов; вынос подкоманд в отдельные файлы размазал бы общий
kernel-транспорт (post, конфиг, константы) без выигрыша в читаемости.

Команды (совместимы с прежним интерфейсом дример-агентов):
  export <docId>                     markdown дока по id
  create "Заголовок" <файл> --parent <hpath>   создать док (ноутбук Projects)
  append <docId> --md <файл> [--force]         дописать markdown-блоки в конец
  replace <docId> --md <файл>        заменить док целиком
  delete <docId> [--force]           снести док в корзину SiYuan (восстановимо)
  orgls                              дерево: ноутбуки + подпапки
  find "тайтл"                       поиск дока по заголовку

delete: без --force сносится только док-заглушка дубля. Заглушка — строка-цитата
markdown, у которой на ОДНОЙ строке есть и «⚠️», и слово «канон»/«заглушк»
(без учёта регистра); смежные строки не объединяются. Guard'ы: док с дочерними
доками сносится только с --force (subtree уходит целиком); канон (без маркера)
— только с --force. ВНИМАНИЕ: --force сносит subtree, доски и тела задач —
для тел задач правильный путь uwuwu-cli task rm, а не delete.

append: идемпотентен при ретраях (инцидент append ×4, 02.11). Перед каждой
отсылкой — контрольный export: нормализованный аппенд совпал с нормализованным
хвостом дока → no-op с репортом «already applied» (rc 0). Нормализация сливает
только рендер-неразличимое (разбеги пробелов и пустые строки вне code-fence)
и ядро-стёртое при roundtrip (whitespace у pipe в таблицах, dash-пэддинг
separator'а — живые пробы 06.11); ведущий отступ вне fence и код внутри fence
сравниваются дословно — ложный no-op на структурно разный markdown невозможен,
промах guard'а всегда даёт видимый дубль, не тихую потерю. Слепых ретраев нет:
export и appendBlock идут attempts=1, единственный ретрай-луп — guard
(APPEND_ATTEMPTS, backoff; худший случай — конверт прежних 6 слепых attempts).
--force — осознанный дубль single-shot: одна отсылка без сверок и ретраев,
сбой — громкая ошибка, повтор — за оператором (слепой луп за флагом вернул бы
инцидент ×N).

orgls: каждый stmt несёт явный limit (ORG_LS_LIMIT) — ядро SiYuan без LIMIT
молча отдаёт первые 64 строки (см. tests/test_wiki_ops_orgls.py).

Конфиг: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) + UWUWU_SIYUAN_TOKEN
(или UWUWU_SIYUAN_WS/conf/conf.json).
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

URL = None
TOKEN = None

# Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки stmt
# (04.11: Experience 859 доков, orgls видел 64). Явный предел обязателен в каждом stmt.
ORG_LS_LIMIT = 100000

# export дока в markdown (cmd_export, _is_stub, _export_content).
EXPORT_MD_PATH = "/api/export/exportMdContent"

# Формат docId ядра SiYuan: 14 цифр (timestamp) + '-' + 7 символов [a-z0-9].
# Валидация до любых SQL-запросов — инъекция/мусор отсекаются на входе.
DOC_ID_RE = re.compile(r"^\d{14}-[a-z0-9]{7}$")

# Бюджет guard-лупа append: единственная точка ретраев (шаги внутри — attempts=1).
# Худший случай 6×(≤60s urlopen) + backoff — конверт прежних 6 слепых attempts post().
APPEND_ATTEMPTS = 6

FENCE_RE = re.compile(r"^\s*```")
# Separator таблицы: только pipe/дефисы/двоеточия/пробелы и обязателен pipe
# (thematic break «---» без pipe — не он).
SEPARATOR_ROW_RE = re.compile(r"(?=.*\|)[\s|:\-]*-[\s|:\-]*")


class KernelRefused(RuntimeError):
    """Ядро ответило бизнес-отказом (code≠0 без «metadata is not ready»).

    Перманентный отказ (несуществующий родитель и т.п.) — ретраи бессмысленны,
    падаем немедленно. Транспортные сбои и metadata-warmup ретраятся как прежде.
    """


def _conf():
    global URL, TOKEN
    URL = (URL or __import__("os").environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")).rstrip("/")
    if not URL.startswith(("http://", "https://")):
        raise RuntimeError(f"UWUWU_SIYUAN_URL должен быть http(s) URL, получено: {URL!r}")
    if TOKEN:
        return
    import os
    tok = os.environ.get("UWUWU_SIYUAN_TOKEN", "")
    if not tok:
        ws = os.environ.get("UWUWU_SIYUAN_WS", str(Path.home() / "siyuan-demo-ws"))
        conf = json.loads((Path(ws) / "conf" / "conf.json").read_text())
        tok = conf["api"]["token"]
    TOKEN = tok


def post(path: str, body: dict, attempts: int = 6):
    _conf()
    last = "?"
    for i in range(attempts):
        req = urllib.request.Request(URL + path, data=json.dumps(body).encode(),
                                     headers={"Authorization": f"Token {TOKEN}",
                                              "Content-Type": "application/json"}, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                d = json.loads(r.read())
        except Exception as e:
            last = str(e)
            if i < attempts - 1:
                time.sleep(1 + i)
            continue
        if d.get("code") == 0:
            return d.get("data")
        last = f"code={d.get('code')} msg={d.get('msg')}"
        if "metadata is not ready" in str(d.get("msg")):
            if i < attempts - 1:
                time.sleep(2 + i)
            continue
        raise KernelRefused(f"POST {path}: {last}")
    raise RuntimeError(f"POST {path}: {last}")


def notebooks(attempts: int = 6) -> list[dict]:
    return [nb for nb in post("/api/notebook/lsNotebooks", {}, attempts=attempts).get("notebooks", [])
            if not nb.get("closed")]


def cmd_export(a):
    r = post(EXPORT_MD_PATH, {"id": a.docId})
    print((r or {}).get("content", ""))


def cmd_create(a):
    md = Path(a.file).read_text()
    nb = next((n["id"] for n in notebooks() if n["name"] == "Projects"), None)
    if not nb:
        raise RuntimeError("ноутбук Projects не найден")
    hpath = a.parent.rstrip("/") + "/" + a.title.strip("/")
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"создан: {hpath}")


def _export_content(doc_id: str) -> str:
    r = post(EXPORT_MD_PATH, {"id": doc_id}, attempts=1)
    return (r or {}).get("content", "")


def _normalized_lines(text: str) -> list[str]:
    """Строки текста в хвост-эквивалентной нормализации (контракт из живых проб).

    Вне code-fence: пустые строки — дроп (ядро разыхивает списки, вставляя их);
    внутренние whitespace-разбеги — схлоп до одного пробела (рендер markdown их
    стирает); ведущий отступ строки — дословно (слипание «без отступа»/«с
    отступом» дало бы тихую потерю записи: top-level vs подсписок, параграф vs
    индет-блок). Строки таблиц (≥2 pipe): whitespace вплотную к pipe — дроп
    (ядро стирает его при roundtrip, проба 1); одиночный pipe в прозе («a | b»)
    идёт по прозаическому правилу — слипание с «a|b» исключено; осознанный
    остаток — многопipe'овая проза (редка). Separator-строка (только pipe/
    дефисы/двоеточия) — канонизируется: ядро пэддит дефисы до ширины колонок
    (живая верификация 06.11: «|-----|» → «| -----|»), скелет двоеточий
    выравнивания сохраняется, внутренние пробелы ячеек не трогаем — слипаем
    меньше, чем известно про ядро. Осознанная граница: дроп пустых строк вне
    fence рендер-различим для tight/loose списков — это слипание вариантов
    одного текста, нужное для детекции ретрая после разыхивания; не «чинить».
    Внутри code-fence — дословно: пробелы кода семантичны.
    """
    normalized: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if FENCE_RE.match(line):
            in_fence = not in_fence
            normalized.append(line.rstrip())
            continue
        if in_fence:
            normalized.append(line)
            continue
        stripped = line.strip()
        if not stripped:
            continue
        leading = line[: len(line) - len(line.lstrip())]
        if SEPARATOR_ROW_RE.fullmatch(stripped):
            normalized.append(leading + _canonical_separator(stripped))
        elif stripped.count("|") >= 2:
            normalized.append(leading + re.sub(r"\s*\|\s*", "|", stripped))
        else:
            normalized.append(leading + " ".join(stripped.split()))
    return normalized


def _canonical_separator(row: str) -> str:
    """Separator таблицы → «|» + скелет двоеточий каждой колонки + «-».

    Пэддинг дефисов ядром нестабилен (ширина колонок), позиции двоеточий —
    семантика выравнивания: «:---:» → «:-:», «----------» → «-».
    """
    cells = [c.strip() for c in row.strip().strip("|").split("|")]
    skeleton = []
    for cell in cells:
        left = ":" if cell.startswith(":") else ""
        right = ":" if cell.endswith(":") and len(cell) > 1 else ""
        skeleton.append(f"{left}-{right}" if cell else "")
    return "|" + "|".join(skeleton) + "|"


def _tail_already_applied(exported: str, appended: str) -> bool:
    app = _normalized_lines(appended)
    if not app:
        return False
    exp = _normalized_lines(exported)
    return len(exp) >= len(app) and exp[-len(app):] == app


def _append_block(doc_id: str, md: str) -> None:
    post("/api/block/appendBlock", {"dataType": "markdown", "data": md, "parentID": doc_id},
         attempts=1)


def cmd_append(a):
    doc_id = a.docId.strip()
    if not DOC_ID_RE.fullmatch(doc_id):
        raise RuntimeError(f"неверный docId: {doc_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    md = Path(a.file).read_text()
    if a.force:
        # Осознанный дубль — single-shot: одна отсылка, без сверок и ретраев
        # (слепой луп за флагом вернул бы инцидент ×N: ядро применило, ответ
        # потерян, ретрай отправил снова). Сбой — громкая ошибка; повтор —
        # за осознавшим это оператором.
        _append_block(doc_id, md)
        print(f"дописано в {doc_id}")
        return
    for attempt in range(1, APPEND_ATTEMPTS + 1):
        try:
            if _tail_already_applied(_export_content(doc_id), md):
                print(f"already applied: {doc_id} — контент уже в хвосте дока, append не выполнялся")
                return
            _append_block(doc_id, md)
            print(f"дописано в {doc_id}")
            return
        except KernelRefused:
            raise
        except RuntimeError as err:
            if attempt == APPEND_ATTEMPTS:
                raise RuntimeError(
                    f"append в {doc_id} не подтвердился после {APPEND_ATTEMPTS} попыток "
                    f"(последний сбой: {err}) — проверь контрольным export: wiki_ops export {doc_id}"
                ) from None
            time.sleep(attempt)


def cmd_replace(a):
    doc_id = a.docId.strip()
    if not DOC_ID_RE.fullmatch(doc_id):
        raise RuntimeError(f"неверный docId: {doc_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    md = Path(a.file).read_text()
    # найти док → пересоздать
    stmt = f"select hpath, box from blocks where type='d' and id='{doc_id}' limit 1"  # nosec B608 — docId валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    if not rows:
        raise RuntimeError("док не найден")
    hpath, box = rows[0]["hpath"], rows[0]["box"]
    nb_name = next((n["name"] for n in notebooks() if n["id"] == box), "Projects")
    post("/api/filetree/removeDocByID", {"id": a.docId})
    time.sleep(1)
    nb = next(n["id"] for n in notebooks() if n["name"] == nb_name)
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"заменён: {hpath}")


def _sql_rows(stmt: str) -> list[dict]:
    rows = post("/api/query/sql", {"stmt": stmt})
    return rows if isinstance(rows, list) else rows.get("data", [])


def _sql_scalar(stmt: str) -> int:
    rows = _sql_rows(stmt)
    return int(rows[0]["c"]) if rows else 0


def _sql_str(value: str) -> str:
    # Одинарная кавычка в SQL-литерале экранируется удвоением (SQLite kernel).
    return value.replace("'", "''")


def _doc_row(doc_id: str) -> dict | None:
    stmt = f"select id, hpath, path, box from blocks where type='d' and id='{doc_id}' limit 1"  # nosec B608 — docId валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    return rows[0] if rows else None


def _children_count(doc_path: str) -> int:
    # Формат blocks.path: сегменты-директории без «.sy», расширение только у листа
    # (/…/<parent>/<child>.sy). Проверено на живом ядре: сегменты — только id.
    parent_prefix = doc_path.removesuffix(".sy")
    stmt = f"select count(*) c from blocks where type='d' and path like '{parent_prefix}/%' limit 1"  # nosec B608 — path из kernel API, только id-сегменты
    return _sql_scalar(stmt)


def _siblings_count(box: str, hpath: str) -> int:
    hpath_sql = _sql_str(hpath)
    stmt = f"select count(*) c from blocks where type='d' and box='{box}' and hpath='{hpath_sql}' limit 1"  # nosec B608 — box из kernel API, hpath экранирован _sql_str
    return _sql_scalar(stmt)


def _is_stub(doc_id: str) -> bool:
    # Заглушка дубля: строка-цитата с «⚠️» И словом «канон»/«заглушк» на той же строке.
    r = post(EXPORT_MD_PATH, {"id": doc_id})
    content = (r or {}).get("content", "")
    for line in content.splitlines():
        if not line.lstrip().startswith(">"):
            continue
        low = line.lower()
        if "⚠️" in line and ("канон" in low or "заглушк" in low):
            return True
    return False


def _notebook_display_name(box_id: str) -> str:
    # Косметика для трейла delete: один запрос без ретраев, отказ ядра
    # не роняет команду после успешного сноса — фолбэк на box-id.
    try:
        return next(n["name"] for n in notebooks(attempts=1) if n["id"] == box_id)
    except Exception:
        return box_id


def cmd_delete(a):
    doc_id = a.docId.strip()
    if not DOC_ID_RE.fullmatch(doc_id):
        raise RuntimeError(f"неверный docId: {doc_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    row = _doc_row(doc_id)
    if not row:
        raise RuntimeError(f"док {doc_id} не найден (уже удалён?) — проверь wiki_ops find")
    children = _children_count(row["path"])
    if children and not a.force:
        raise RuntimeError(f"у дока «{row['hpath']}» есть дочерние доки ({children}) — снос унесёт subtree; подтверди --force")
    siblings = _siblings_count(row["box"], row["hpath"])
    stub = _is_stub(doc_id)
    if not stub and not a.force:
        if siblings <= 1:
            raise RuntimeError(f"«{row['hpath']}» — единственный док по пути и без маркера заглушки (канон); снос только с --force")
        raise RuntimeError(f"док без маркера заглушки; по пути ещё {siblings - 1} дубл(ей) — погаси заглушкой (replace + маркер «⚠️ … Канон — …») или подтверди --force")
    post("/api/filetree/removeDocByID", {"id": doc_id}, attempts=2)
    for _ in range(2):
        if _doc_row(doc_id) is None:
            marker = "да" if stub else "нет (--force)"
            print(f"снесён: {doc_id} {row['hpath']} (ноутбук {_notebook_display_name(row['box'])})")
            print(f"  маркер заглушки: {marker}; соседей по пути: {siblings}; дочерних доков: {children}")
            print("  → корзина SiYuan, восстановимо через UI (история данных) до истечения срока хранения")
            return
        time.sleep(1)
    raise RuntimeError("removeDocByID вернул ok, но док остался в индексе — проверь состояние дока вручную")


def cmd_orgls(a):
    for nb in notebooks():
        print(f"[{nb['name']}]")
        stmt = f"select hpath from blocks where type='d' and box='{nb['id']}' limit {ORG_LS_LIMIT}"  # nosec B608 — nb.id из kernel API
        rows = _sql_rows(stmt)
        for r in sorted(rows, key=lambda x: x["hpath"]):
            depth = r["hpath"].count("/") - 1
            print("  " * (depth + 1) + r["hpath"].rstrip("/").split("/")[-1])


def cmd_find(a):
    stmt = f"select id, hpath from blocks where type='d' and content like '%{_sql_str(a.title)}%' limit 10"  # nosec B608 — title экранирован _sql_str
    rows = _sql_rows(stmt)
    for r in rows:
        print(r["id"], r["hpath"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("export")
    p.add_argument("docId")
    p.set_defaults(fn=cmd_export)
    p = sub.add_parser("create")
    p.add_argument("title")
    p.add_argument("file")
    p.add_argument("--parent", default="")
    p.set_defaults(fn=cmd_create)
    p = sub.add_parser("append")
    p.add_argument("docId")
    p.add_argument("--md", dest="file", required=True)
    p.add_argument("--force", action="store_true",
                   help="осознанный дубль single-shot: одна отсылка без guard'а и ретраев")
    p.set_defaults(fn=cmd_append)
    p = sub.add_parser("replace")
    p.add_argument("docId")
    p.add_argument("--md", dest="file", required=True)
    p.set_defaults(fn=cmd_replace)
    p = sub.add_parser("delete")
    p.add_argument("docId")
    p.add_argument("--force", action="store_true")
    p.set_defaults(fn=cmd_delete)
    p = sub.add_parser("orgls")
    p.set_defaults(fn=cmd_orgls)
    p = sub.add_parser("find")
    p.add_argument("title")
    p.set_defaults(fn=cmd_find)
    a = ap.parse_args()
    try:
        a.fn(a)
    except RuntimeError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
