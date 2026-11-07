#!/usr/bin/env python3
"""wiki_ops — операции с доками uwuwu (SiYuan-бекенд). Единый CLI-модуль всех
wiki-операций агентов; вынос подкоманд в отдельные файлы размазал бы общий
kernel-транспорт (post, конфиг, константы) без выигрыша в читаемости.

Один файл по контракту: dreamer/triage ссылаются на путь tools/wiki_ops.py напрямую
(см. dreamer/dreamer.ts) — разнесение по модулям сломает их пайплайны.

Команды (совместимы с прежним интерфейсом дример-агентов):
  export <docId>                     markdown дока по id
  create "Заголовок" <файл> --parent <путь>   создать док; ноутбук резолвится по первому
                                   сегменту пути: имя ноутбука (/Projects/uwuwu/ADR →
                                   Projects + hpath /uwuwu/ADR, /Experience/cli →
                                   Experience + hpath /cli) или корневая папка ровно
                                   одного ноутбука (/uwuwu/ADR → Projects, /affine →
                                   Experience); пустой --parent → Projects (легаси).
                                   Гард номера ADR: живой SQL-find по папке-приёмнику —
                                   занятый «ADR-NNN» в тайтле даёт предупреждение в stderr
                                   (гонка параллельных дримеров 06.11), создание продолжается
  append <docId> --md <файл> [--force]         дописать markdown-блоки в конец
  replace <docId> --md <файл> [--base <снапшот>] [--force]
                                   заменить док целиком; --base — снапшот
                                   свежести (нетронутый вывод export до правки)
  delete <docId> [--force]           снести док в корзину SiYuan (восстановимо)
  orgls                              дерево: ноутбуки + подпапки
  find "тайтл"                       поиск дока по заголовку

Блочная хирургия — точечная правка rich-доков по blockID (doc-level append умеет
только «в конец», replace пересоздаёт док и теряет привязки/живые HTML-блоки):
  block-update <blockID> --md <файл>     правка блока; payload строго один блок —
                                         multi-block ядро молча обрезает до первого;
                                         таблица (type='t') — только payload полной
                                         таблицы (шапка + separator + все строки):
                                         update переписывает таблицу ЦЕЛИКОМ,
                                         row-payload схлопывает её тихо, code=0
                                         (пробы 06.11/07.11)
  block-insert-after <blockID> --md <файл>   вставка после блока (multi-block легален);
                                         на 't' — сиблинг после таблицы, таблица цела
                                         (проба 07.11)
  block-insert-html <docId|blockID> --html <файл> [--after-marker <имя>]
                                         вставка живого HTML-блока (dataType="dom",
                                         dom мимо markdown-парсера — тот деградирует
                                         длинные/сложные div'ы до параграфов). Без
                                         маркера — сразу после блока (паритет
                                         block-insert-after); с --after-marker — после
                                         маркер-абзаца «⁂ diagram:<имя>» (ADR-052) в
                                         доке: маркер уникален (0/>1 — отказ с
                                         раскладкой), сосед NodeHTMLBlock в .sy →
                                         отказ «уже вставлена» (обновление:
                                         block-delete соседа → повтор). Пост-чек:
                                         SQL type='html' по новым id с фолбэком на
                                         .sy (FS-истина) — деградация не молчалива
  block-delete <blockID>                 снос leaf-блока; контейнер с детьми или
                                         таблица — отказ (ядро сносит контекст целиком
                                         и тихо; снос list — update контейнера:
                                         list→параграф; снос таблицы — doc-level
                                         replace; item list ядро не оставляет без
                                         ребёнка — посерийный снос item'ов невозможен)

Guard'ы блочных команд до любой мутации: формат id до любого SQL; блок резолвится
в индексе (LIMIT 1); doc-id (type='d') — отказ всем трём (update на doc-id молча
стирает весь док; правка/снос дока — append/replace/delete). Каждый stmt несёт
явный LIMIT. Мутация — один сетевой вызов (post attempts=1) без авто-ретрая:
ретрай после потери ответа — класс дубля вставки, не самоисправление. Undo-журнал
пишется ДО мутации через kernel putFile в data/assets/undo/wiki_ops/<unixts>-<op>-<id>.json
(before_md / after_md / payload_md / new_ids): ядро пишет файл под своим uid — прямой
FS-доступ с хоста к data/assets невозможен (проба 06.11), зато журнал работает с любой
машины, где доступно ядро. Не записался — warning, операция не падает.

delete: без --force сносится только док-заглушка дубля. Заглушка — строка-цитата
markdown, у которой на ОДНОЙ строке есть и «⚠️», и слово «канон»/«заглушк»
(без учёта регистра); смежные строки не объединяются. Guard'ы: док с дочерними
доками сносится только с --force (subtree уходит целиком); канон (без маркера)
— только с --force. Пост-чек: вердикт — по файловой пробе ядра (getFile
data/<box><path>, FS-истина — ядро читает диск, ADR-025); SQL-индекс отстаёт
от мутаций секундами (Experience/cli write-path) — примечание к успеху или
фолбэк при failed-пробе, успех не отвергает. ВНИМАНИЕ: --force сносит subtree,
доски и тела задач — для тел задач правильный путь uwuwu-cli task rm, а не delete.

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

replace: полный rewrite — guard свежести по снапшоту (инцидент 05.11: rewrite
«Стек и архитектура» пересоздал док и тихо затёр параллельный append другой
сессии). Без --base и без --force — отказ с рецептом (тихая потеря чужого
append невидима ни одному из параллельных писателей). --base — нетронутый
вывод export, снятый до правки: exportMdContent байт-стабилен между вызовами
и несёт updated дока в lastmod frontmatter (живые пробы 06.11), сверка со
свежим export ловит любую правку после снапшота, не только bump updated;
расхождение — отказ до removeDocByID (ничего не снесено) с рецептом «перечитай,
влей дельту, пересними снапшот». Нормализация _normalized_lines гасит
рендер-шум ядра. --force старше --base: осознанный overwrite single-shot без
сверок, по образцу append --force. Остаточное TOCTOU-окно (секунды между
сверкой и сносом) неустранимо без CAS ядра — как у append guard'а (ADR-038).

orgls: каждый stmt несёт явный limit (ORG_LS_LIMIT) — ядро SiYuan без LIMIT
молча отдаёт первые 64 строки (см. tests/test_wiki_ops_orgls.py).

post(): «metadata is not ready» — ≤3 попытки (+2s/+5s) → различимый bail с
хинтом (дисциплина ADR-027: волна гаснет отходом клиента, ретраи — её топливо);
транспортные ошибки — ретраи с бэкоффом (sleep-free на последней попытке);
прочие code≠0 — перманентный отказ ядра, KernelRefused немедленно (ретраи
бессмысленны, см. класс ниже).

Конфиг: UWUWU_SIYUAN_URL (деф. http://127.0.0.1:6806) + UWUWU_SIYUAN_TOKEN
(или UWUWU_SIYUAN_WS/conf/conf.json).
"""
from __future__ import annotations

import argparse
import html as html_module
import json
import os
import re
import secrets
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import urlparse

URL = None
TOKEN = None

# Ядро /api/query/sql без явного LIMIT молча отдаёт первые 64 строки stmt
# (04.11: Experience 859 доков, orgls видел 64). Явный предел обязателен в каждом stmt.
ORG_LS_LIMIT = 100000

# export дока в markdown (cmd_export, _is_stub, _export_content).
EXPORT_MD_PATH = "/api/export/exportMdContent"

# getFile-проба файла воркспейса (пост-чек delete, ADR-025 файловый контракт).
GET_FILE_PATH = "/api/file/getFile"

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

# «metadata is not ready»: ≤3 попытки с паузами +2s/+5s → различимый bail с хинтом.
# Дисциплина ADR-027 (uwuwu-cli): волна деградации гаснет отходом клиента — слепые
# ретраи питают волну, но полный запрет ретраев убивает выживание на транзиентном
# лаге индекса после крупных вставок. Арит-окно инстанса не переносится: wiki_ops
# выполняет одну write-операцию на процесс — межвызовного долбления нет.
# Guard-луп append это не обходит: его шаги идут attempts=1 (bail-счётчик внутри
# одного post() не достигает порога), бюджет ретраев держит сам луп.
METADATA_BAIL_ATTEMPTS = 3
METADATA_BAIL_BACKOFF = (2, 5)


class KernelRefused(RuntimeError):
    """Ядро ответило бизнес-отказом (code≠0 без «metadata is not ready»).

    Перманентный отказ (несуществующий родитель и т.п.) — ретраи бессмысленны,
    падаем немедленно. Транспортные сбои и metadata-warmup ретраятся как прежде.
    """


def _conf():
    global URL, TOKEN
    URL = (URL or os.environ.get("UWUWU_SIYUAN_URL", "http://127.0.0.1:6806")).rstrip("/")
    if not URL.startswith(("http://", "https://")):
        raise RuntimeError(f"UWUWU_SIYUAN_URL должен быть http(s) URL, получено: {URL!r}")
    if TOKEN:
        return
    tok = os.environ.get("UWUWU_SIYUAN_TOKEN", "")
    if not tok:
        ws = os.environ.get("UWUWU_SIYUAN_WS", str(Path.home() / "siyuan-demo-ws"))
        conf = json.loads((Path(ws) / "conf" / "conf.json").read_text())
        tok = conf["api"]["token"]
    TOKEN = tok


def _metadata_hint(api_path: str) -> str:
    # Хинт различимого bail'а (ADR-027). Причины metadata-not-ready зависят от пути:
    # createDocWithMd падает и на отсутствующем родителе/спецсимволах тайтла
    # (опыт /cli/SiYuan createDocWithMd…), прочие пути — почти всегда волна.
    if api_path.endswith("createDocWithMd"):
        return ("если это create/replace — путь-родитель не существует в целевом ноутбуке "
                "(createDocWithMd не создаёт родителей) или в тайтле спецсимволы («/», «=», …); "
                "иначе — волна деградации ядра: uwuwu-cli doctor")
    return "вероятна волна деградации ядра: uwuwu-cli doctor"


def _post_once(path: str, body: dict) -> tuple[str, object, str]:
    """Один POST к ядру. Возвращает (kind, data, last):
    kind — ok | network | metadata | code; last — текст последней ошибки."""
    req = urllib.request.Request(URL + path, data=json.dumps(body).encode(),
                                 headers={"Authorization": f"Token {TOKEN}",
                                          "Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:  # nosec B310 — схема http/https из UWUWU_SIYUAN_URL
            d = json.loads(r.read())
    except Exception as e:
        return "network", None, str(e)
    if d.get("code") == 0:
        return "ok", d.get("data"), ""
    last = f"code={d.get('code')} msg={d.get('msg')}"
    if "metadata is not ready" in str(d.get("msg")):
        return "metadata", None, last
    return "code", None, last


def _retry_sleep(attempt_index: int, attempts: int, seconds: float) -> None:
    """Пауза перед повтором; на последнем attempt сна нет — raise сразу
    (terminal-sleep снят по ревью круга 1)."""
    if attempt_index < attempts - 1:
        time.sleep(seconds)


def post(path: str, body: dict, attempts: int = 6):
    _conf()
    last = "?"
    metadata_tries = 0
    for i in range(attempts):
        kind, data, last = _post_once(path, body)
        if kind == "ok":
            return data
        if kind == "network":
            # транспортный флейк — лечится повтором (пин test_transport_error_keeps_full_retries)
            _retry_sleep(i, attempts, 1 + i)
            continue
        if kind == "metadata":
            metadata_tries += 1
            if metadata_tries >= METADATA_BAIL_ATTEMPTS:
                raise RuntimeError(f"POST {path}: {last} — metadata-bail после "
                                   f"{metadata_tries} попыток; {_metadata_hint(path)}")
            _retry_sleep(i, attempts, METADATA_BAIL_BACKOFF[metadata_tries - 1])
            continue
        # перманентный code≠0 — KernelRefused fail-fast (merge PR #80): ретраи
        # бессмысленны, повтор отклонённой мутации вреден; блочные мутации
        # и так single-shot (attempts=1 на call-сайтах)
        raise KernelRefused(f"POST {path}: {last}")
    raise RuntimeError(f"POST {path}: {last}")


def _fs_file(path: str) -> str:
    """getFile-проба файла воркспейса (контракт ядра 3.8.9, ADR-025): один
    выстрел без ретраев — проба существования должна быть быстрой и честной.
    "found" — HTTP 200 с байтами (ядро читает диск, индекс не участвует);
    "missing" — конверт code 404 (файла нет, приходит как HTTP 202);
    "failed" — проба не состоялась: транспорт, прочий код, не-JSON, 200 с
    пустым телом (пустое тело это промах, не успех — правило write-path).
    """
    _conf()
    req = urllib.request.Request(URL + GET_FILE_PATH, data=json.dumps({"path": path}).encode(),
                                 headers={"Authorization": f"Token {TOKEN}",
                                          "Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:  # nosec B310 — URL из _conf (scheme-guard http/https), путь — константа kernel API
            body = r.read()
            status = getattr(r, "status", None)  # неизвестный статус → не 200: конверт/failed, не «found»
    except Exception:
        return "failed"
    if status == 200:
        return "found" if body else "failed"
    try:
        envelope = json.loads(body)
    except Exception:
        return "failed"
    return "missing" if envelope.get("code") == 404 else "failed"


def notebooks(attempts: int = 6) -> list[dict]:
    """Конверт lsNotebooks обязан нести dict с list-полем "notebooks":
    аномалия (None, dict без поля) — RuntimeError. Тихий [] здесь читался бы
    как «пустой воркспейс» — та же маскировка сбоя под not-found, от которой
    защищены SQL-конверты (задача 20261104072714, ревью PR #111 round-2).
    Пустой notebooks (все закрыты/ни одного) — легитимный not-found."""
    data = post("/api/notebook/lsNotebooks", {}, attempts=attempts)
    if not (isinstance(data, dict) and isinstance(data.get("notebooks"), list)):
        raise RuntimeError(
            f"lsNotebooks вернул аномальный конверт (ожидался dict с list-полем notebooks): {data!r:.200}")
    return [nb for nb in data["notebooks"] if not nb.get("closed")]


def cmd_export(a):
    r = post(EXPORT_MD_PATH, {"id": a.docId})
    print((r or {}).get("content", ""))


def path_safe(title: str) -> str:
    """Тайтл → безопасный сегмент hpath: сырой «/» в нём — разделитель пути,
    ядро молча строит цепочку доков-оболочек (инцидент 06.11, /devops).
    Канвенция миграции bind_rows / cli path_safe (PR #36): «/» → U+2215."""
    return title.replace("/", "\u2215")


def cmd_create(a):
    md = Path(a.file).read_text()
    nb_id, nb_name, prefix = _resolve_parent(a.parent)
    title = path_safe(a.title.strip("/"))
    if not title:
        raise RuntimeError(f"пустой тайтл: {a.title!r} состоит из одних слэшей — "
                           "имя дока не может быть пустым")
    hpath = prefix.rstrip("/") + "/" + title
    _warn_if_adr_number_taken(nb_id, prefix.rstrip("/"), title)
    post("/api/filetree/createDocWithMd", {"notebook": nb_id, "path": hpath, "markdown": md})
    print(f"создан: [{nb_name}] {hpath}")


ADR_NUMBER_RE = re.compile(r"ADR-(\d{2,4})\b")


def _adr_number(title: str) -> int | None:
    match = ADR_NUMBER_RE.search(title)
    return int(match.group(1)) if match else None


def _warn_if_adr_number_taken(notebook_id: str, folder_hpath: str, title: str) -> None:
    """Живой find номера ADR в папке-приёмнике до создания дока (профилактика гонки).

    Гонка параллельных дримеров реальна: двойные ADR-033/037/040 за один день
    (06.11) — orgls-срез у каждого автора был до создания чужого дока. Номер
    держит первый по времени (ADR-013). Проба косметическая: отказ ядра не
    роняет создание — только глушит проверку; находка — предупреждение в stderr,
    не отказ (занятый номер бывает осознанным переездом дока).
    """
    number = _adr_number(title)
    if number is None:
        return
    stmt = (f"select content, hpath from blocks where type='d' and box='{notebook_id}' "
            f"and hpath like '{_sql_str(folder_hpath)}/%' limit 200")  # nosec B608 — box из kernel API, hpath экранирован _sql_str
    try:
        rows = _sql_rows(stmt, attempts=1)
    except Exception:
        print("ПРЕДУПРЕЖДЕНИЕ: проба занятости ADR-номера не состоялась (ядро) — "
              'сверь папку руками: wiki_ops.py find "<тайтл>"', file=sys.stderr)
        return
    taken_by = [r["hpath"] for r in rows if _adr_number(r.get("content") or "") == number]
    if taken_by:
        print(f"ПРЕДУПРЕЖДЕНИЕ: номер ADR-{number:03d} уже занят в {folder_hpath}: "
              f"{'; '.join(taken_by)} — номер держит первый по времени (ADR-013); "
              f"создание продолжено", file=sys.stderr)


def _normalize_parent(parent: str) -> str:
    # Дикие формы из живых инцидентов: «Experience/tools» без ведущего слэша,
    # «/affine/» с хвостовой, задвоенные слэши.
    normalized = re.sub(r"/{2,}", "/", parent.strip())
    if not normalized.startswith("/"):
        normalized = "/" + normalized
    return normalized.rstrip("/") or "/"


def _workspace_roots(nbs: list[dict]) -> dict[str, list[str]]:
    """Имена корневых доков каждого открытого ноутбука (для резолва первого сегмента)."""
    roots: dict[str, list[str]] = {}
    for nb in nbs:
        files = post("/api/filetree/listDocsByPath", {"notebook": nb["id"], "path": "/"}).get("files") or []
        roots[nb["name"]] = [f["name"] for f in files]
    return roots


def _resolve_parent(parent: str) -> tuple[str, str, str]:
    """--parent → (notebook_id, notebook_name, hpath-префикс дока).

    Первый сегмент пути резолвится в ноутбук: точное имя ноутбука (hpath — остаток
    пути) либо имя корневого дока ровно одного ноутбука (hpath — путь как есть).
    Неоднозначный корень и неизвестный префикс — RuntimeError с вариантами.
    Пустой путь — легаси-дефолт Projects (пустой --parent у старых вызовов).
    """
    normalized = _normalize_parent(parent)
    nbs = notebooks()
    by_name = {nb["name"]: nb["id"] for nb in nbs}
    if normalized == "/":
        if "Projects" not in by_name:
            raise RuntimeError("ноутбук Projects не найден — легаси-дефолт пустого --parent недоступен")
        return by_name["Projects"], "Projects", "/"
    parts = normalized[1:].split("/", 1)
    first = parts[0]
    rest = "/" + parts[1] if len(parts) > 1 else "/"
    if first in by_name:
        return by_name[first], first, rest
    owners = sorted(name for name, names in _workspace_roots(nbs).items() if first in names)
    if len(owners) == 1:
        return by_name[owners[0]], owners[0], normalized
    if owners:
        options = " или ".join(f"/{o}/{first}" for o in owners)
        raise RuntimeError(f"корневая папка «{first}» есть в нескольких ноутбуках "
                           f"({', '.join(owners)}) — уточни: {options}")
    names = ", ".join(sorted(by_name))
    raise RuntimeError(f"неизвестный префикс «{normalized}»: первый сегмент --parent должен быть "
                       f"именем ноутбука ({names}) или его корневой папкой (состав — orgls); "
                       f"примеры: /Experience/cli, /uwuwu/ADR")


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


def _require_fresh_snapshot(doc_id: str, base_file: str | None) -> None:
    """Guard lost-update перед remove+create (инцидент 05.11: полный rewrite
    «Стек и архитектура» затёр параллельный append другой сессии). Снапшот
    --base — нетронутый вывод wiki_ops export, снятый до правки: exportMdContent
    байт-стабилен между вызовами и несёт updated дока в lastmod frontmatter
    (живые пробы 06.11), поэтому сравнение со свежим export ловит любую правку
    после снапшота, не только bump updated. Нормализация _normalized_lines гасит
    известный рендер-шум ядра — ложный отказ на whitespace-дрейфе невозможен.
    Отказ — строго до первой мутации (removeDocByID ещё не звался). Остаточное
    TOCTOU-окно (секунды между сверкой и сносом) неустранимо без CAS ядра —
    прецедент: append guard (ADR-038)."""
    if base_file is None:
        raise RuntimeError(
            "replace требует --base <снапшот.md> — нетронутый вывод wiki_ops export, "
            "снятый до правки (workflow: export → правь копию → replace --base снапшот), "
            "или --force для осознанного overwrite без сверки: без снапшота параллельный "
            "append другой сессии теряется тихо (инцидент 05.11, «Стек и архитектура»)"
        )
    base_text = Path(base_file).read_text()
    try:
        current = _export_content(doc_id)
    except RuntimeError as exc:
        raise RuntimeError(
            f"guard свежести не состоялся — export не читается ({exc}); "
            "replace не выполнялся, повтори операцию целиком"
        ) from None
    if _normalized_lines(base_text) != _normalized_lines(current):
        raise RuntimeError(
            f"док {doc_id} менялся после снапшота (--base) — replace затёр бы чужие правки. "
            f"Перечитай текущий контент: wiki_ops export {doc_id}; влей дельту в новый "
            "контент, пересними снапшот и повтори; --force — overwrite без сверки"
        )


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
    # guard свежести до первой мутации; --force старше --base — осознанный
    # overwrite идёт без сверок, как append --force
    if not a.force:
        _require_fresh_snapshot(doc_id, a.base)
    hpath, box = rows[0]["hpath"], rows[0]["box"]
    nb_name = next((n["name"] for n in notebooks() if n["id"] == box), "Projects")
    post("/api/filetree/removeDocByID", {"id": a.docId})
    time.sleep(1)
    nb = next(n["id"] for n in notebooks() if n["name"] == nb_name)
    post("/api/filetree/createDocWithMd", {"notebook": nb, "path": hpath, "markdown": md})
    print(f"заменён: {hpath}")


def _sql_rows(stmt: str, attempts: int = 6) -> list[dict]:
    """Конверт /api/query/sql обязан нести список строк: list — как есть;
    dict с list-полем "data" — unwrap (совместимость); всё остальное —
    аномальный конверт ядра. Пустой список здесь честный not-found (SELECT
    не нашёл строк), аномалия — нет: молчаливый [] маскирует сбой под
    «строк нет» (задача 20261104072714)."""
    rows = post("/api/query/sql", {"stmt": stmt}, attempts=attempts)
    if isinstance(rows, list):
        return rows
    if isinstance(rows, dict) and isinstance(rows.get("data"), list):
        return rows["data"]
    raise RuntimeError(f"SQL вернул аномальный конверт (ожидался список строк): {rows!r:.200}")


def _sql_scalar(stmt: str) -> int:
    # count-запрос на здоровом ядре всегда возвращает ровно одну строку:
    # пустота = сбой, fail-closed (тихий 0 у delete-guard'ов означал бы
    # «сноси без --force»).
    rows = _sql_rows(stmt)
    if not rows:
        raise RuntimeError(f"count-запрос вернул 0 строк (ожидалась ровно одна): {stmt}")
    return int(rows[0]["c"])


def _sql_str(value: str) -> str:
    # Одинарная кавычка в SQL-литерале экранируется удвоением (SQLite kernel).
    return value.replace("'", "''")


def _doc_row(doc_id: str, attempts: int = 6) -> dict | None:
    stmt = f"select id, hpath, path, box from blocks where type='d' and id='{doc_id}' limit 1"  # nosec B608 — docId валидирован DOC_ID_RE
    rows = _sql_rows(stmt, attempts)
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


def _fs_verdict_note(doc_id: str) -> str:
    # Примечание к FS-вердикту: расхождение SQL (лаг индекса) — не ошибка.
    # Косметика: attempts=1, отказ пробы не роняет успешный снос.
    try:
        index_still_sees = _doc_row(doc_id, attempts=1) is not None
    except Exception:
        index_still_sees = False
    if index_still_sees:
        return "вердикт по файловой пробе; SQL-индекс ещё видит док — лаг, сойдётся сам"
    return "вердикт по файловой пробе"


def _doc_fs_path(row: dict) -> str:
    """FS-путь файла дока: data/<box><blocks.path>. Box — каталог данных
    ноутбука, в blocks.path НЕ входит (path — id-цепочка от корня:
    /<rootId>/<…>/<docId>.sy); контракт сверен живым ядром (getFile: с box —
    Bytes, без — 404) и канонической сборкой doc_file_path в cli/src/siyuan.rs
    (ADR-025, задача 20261106044426)."""
    return f"data/{row['box']}{row['path']}"


def _confirm_removed(row: dict, doc_id: str) -> str:
    """Вердикт пост-чека после ok от removeDocByID: источник вердикта строкой.

    Файловая проба — истина (ядро читает диск, SQL-индекс не участвует);
    индекс отстаёт от мутаций секундами (Experience/cli write-path) — его
    показание даёт примечание или фолбэк, успех не отвергает (принцип
    ADR-025: один лагающий источник ≠ истина). FS-путь — по _doc_fs_path
    (data/<box><path>), захвачен до мутации.
    """
    fs_path = _doc_fs_path(row)
    for _ in range(2):
        probe = _fs_file(fs_path)
        if probe == "found":
            raise RuntimeError(f"removeDocByID вернул ok, но файл дока остался на диске ({fs_path}) — проверь состояние дока вручную")
        if probe == "missing":
            return _fs_verdict_note(doc_id)
        time.sleep(1)  # проба не состоялась — транспортный транзиент, не деградация данных
    # Файловая проба не сошлась (деградация) → фолбэк на SQL-индекс с примечанием
    for _ in range(2):
        if _doc_row(doc_id) is None:
            return "вердикт по SQL-индексу (файловая проба не состоялась)"
        time.sleep(1)
    raise RuntimeError("removeDocByID вернул ok, но верификация не сошлась: файловая проба "
                       "не состоялась, индекс ещё видит док — проверь вручную "
                       "(uwuwu-cli kernel file, uwuwu-cli doctor)")


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
    verdict = _confirm_removed(row, doc_id)
    marker = "да" if stub else "нет (--force)"
    print(f"снесён: {doc_id} {row['hpath']} (ноутбук {_notebook_display_name(row['box'])})")
    print(f"  маркер заглушки: {marker}; соседей по пути: {siblings}; дочерних доков: {children}")
    print("  → корзина SiYuan, восстановимо через UI (история данных) до истечения срока хранения")
    print(f"  ({verdict})")


# --- блочная хирургия: block-update / block-insert-after / block-insert-html / block-delete ---

def _block_row(block_id: str) -> dict | None:
    stmt = f"select id, type, root_id, parent_id, markdown from blocks where id='{block_id}' limit 1"  # nosec B608 — blockID валидирован DOC_ID_RE
    rows = _sql_rows(stmt)
    return rows[0] if rows else None


def _block_children_count(parent_id: str) -> int:
    stmt = f"select count(*) c from blocks where parent_id='{parent_id}' limit 1"  # nosec B608 — parent_id из kernel API (валидирован на входе)
    return _sql_scalar(stmt)


def _root_hpath(root_id: str) -> str:
    stmt = f"select hpath from blocks where type='d' and id='{root_id}' limit 1"  # nosec B608 — root_id из kernel API
    rows = _sql_rows(stmt)
    return rows[0]["hpath"] if rows else ""


def _payload_has_table_separator(markdown: str) -> bool:
    """Payload несёт separator-строку GFM-таблицы вне code-fence — маркер
    «полной таблицы» для гварда type='t' (updateBlock на 't' переписывает
    таблицу целиком, пробы 06.11/07.11). Построчно с fullmatch: наивный search
    матчит дефис в data-строке («| 2026-11-07 | …», ревью плана 07.11);
    separator внутри fence — конверсия таблицы в код-блок, не таблица. Фенсы —
    через _fence_transition чанк-парсера (backtick и tilde, закрытие только
    тем же символом; ревью кода 07.11: FENCE_RE ловил только backticks)."""
    in_fence = False
    fence_char = ""
    for line in markdown.splitlines():
        transition = _fence_transition(line, in_fence, fence_char)
        if transition == "open":
            in_fence, fence_char = True, line[0]
            continue
        if transition == "close":
            in_fence = False
            continue
        if in_fence:
            continue
        if SEPARATOR_ROW_RE.fullmatch(line.strip()):
            return True
    return False


def _resolved_block(block_id: str) -> dict:
    """Формат id до любого SQL (инъекция/мусор отсечены), резолв в индексе,
    doc-id (type='d') — не блок: update на doc-id молча стирает весь док."""
    if not DOC_ID_RE.fullmatch(block_id):
        raise RuntimeError(f"неверный blockID: {block_id!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    row = _block_row(block_id)
    if not row:
        raise RuntimeError(f"блок {block_id} не найден в индексе — возьми id из wiki_ops export или SQL по доку")
    if row["type"] == "d":
        raise RuntimeError("doc-id не блок: правка контента дока — append/replace, снос дока — wiki_ops delete")
    return row


def _mutate(path: str, body: dict) -> dict:
    """Мутация блока — один сетевой вызов без авто-ретрая: ретрай после потери
    ответа даёт дубль вставки (не самоисправляется). Ошибка несёт рецепт проверки
    состояния до повтора."""
    try:
        return post(path, body, attempts=1)
    except RuntimeError as exc:
        raise RuntimeError(f"{exc}; применение не подтверждено — проверь блок (export/SQL) до повтора операции") from None


CHUNK_FENCE_RE = re.compile(r"^(`{3,}|~{3,}).*$")  # парсер блок-стартов: fence от колонки 0 (индент = продолжение)
HEADING_RE = re.compile(r"#{1,6}\s")
BREAK_RE = re.compile(r"(-{3,}|\*{3,}|_{3,})$")
QUOTE_RE = re.compile(r">")
BULLET_RE = re.compile(r"[-*+]\s")
ORDERED_RE = re.compile(r"\d{1,9}[.)]\s")
TABLE_RE = re.compile(r"\|")


def _is_closing_fence(line: str, fence_char: str) -> bool:
    return line[0] == fence_char and set(line) == {fence_char}


def _fence_transition(line: str, in_code: bool, fence_char: str) -> str | None:
    """'open' / 'close' / None для строки-фенса относительно состояния кода."""
    if not CHUNK_FENCE_RE.match(line):
        return None
    if in_code:
        return "close" if _is_closing_fence(line, fence_char) else None
    return "open"


def _marker_kind(line: str) -> str | None:
    """Тип топ-уровневой конструкции-маркера или None (продолжение абзаца)."""
    if QUOTE_RE.match(line):
        return "quote"
    if BULLET_RE.match(line) or ORDERED_RE.match(line):
        return "list"
    if TABLE_RE.match(line):
        return "table"
    return None


def _starts_new_block(raw_line: str, line: str, chunk_kind: str) -> bool:
    """Новый блок по правилам ядра (Lute): конструкция впритык после абзаца
    (fence/заголовок/цитата/список/таблица/разрыв — проба ревью 05.11: updateBlock
    молча выбрасывает хвост таких payload'ов); строки с отступом — продолжение
    текущего блока (вложенные списки, ленивые переносы)."""
    if raw_line[:1].isspace():
        return False
    if HEADING_RE.match(line) or BREAK_RE.search(line):
        return True  # заголовок/тематический разрыв начинают новый блок всегда
    kind = _marker_kind(line)
    return kind is not None and kind != chunk_kind


def _payload_chunk_count(markdown: str) -> int:
    """Число топ-уровневых блоков в markdown: пустая строка ИЛИ конструкция
    впритык после абзаца начинает новый блок; fenced-код держит пустые строки
    внутри одного блока; одно-родственные строки (items списка, строки цитаты/
    таблицы) — один блок. Ошибки парсера уходят в безопасную сторону — лишний
    отказ вместо тихой потери хвоста."""
    chunk_count = 0
    in_code = False
    fence_char = ""
    inside_chunk = False
    chunk_kind = ""
    for raw_line in markdown.splitlines():
        line = raw_line.strip()
        transition = _fence_transition(line, in_code, fence_char)
        if transition == "open":
            in_code, fence_char = True, line[0]
            chunk_count += 1
            inside_chunk, chunk_kind = True, "fence"
            continue
        if transition == "close":
            in_code = False
            # закрытый fence завершает блок: следующая строка — новый блок
            inside_chunk, chunk_kind = False, ""
            continue
        if in_code:
            continue
        if not line:
            inside_chunk, chunk_kind = False, ""
            continue
        if _starts_new_block(raw_line, line, chunk_kind):
            chunk_count += 1
            inside_chunk, chunk_kind = True, _marker_kind(line) or "paragraph"
            continue
        if not inside_chunk:
            chunk_count += 1
            inside_chunk, chunk_kind = True, "paragraph"
    return chunk_count


UNDO_DIR = "data/assets/undo/wiki_ops"


def _get_workspace_file(ws_path: str) -> bytes:
    """Сырое чтение файла воркспейса через getFile (FS-истина ядра, ADR-025).
    FileNotFoundError — конверт code 404 (файла нет); RuntimeError — транспорт,
    прочий код, не-JSON, 200 без байт. Причины различимы намеренно: транспортный
    сбой ≠ отсутствие файла (ревью плана round-1)."""
    _conf()
    req = urllib.request.Request(URL + GET_FILE_PATH, data=json.dumps({"path": ws_path}).encode(),
                                 headers={"Authorization": f"Token {TOKEN}",
                                          "Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:  # nosec B310 — URL из _conf (scheme-guard http/https), путь — константа kernel API
            body = r.read()
            status = getattr(r, "status", None)
    except Exception as exc:
        raise RuntimeError(f"getFile {ws_path}: {exc}") from None
    if status == 200:
        if not body:
            raise RuntimeError(f"getFile {ws_path}: 200 без байт — промах, не успех")
        return body
    try:
        envelope = json.loads(body)
    except Exception:
        raise RuntimeError(f"getFile {ws_path}: не-JSON ответ (status={status})") from None
    if envelope.get("code") == 404:
        raise FileNotFoundError(ws_path)
    raise RuntimeError(f"getFile {ws_path}: code={envelope.get('code')} msg={envelope.get('msg')}")


def _put_workspace_file(ws_path: str, payload: bytes) -> None:
    """Запись файла в воркспейс, контракт ядра 3.8.9: POST multipart (path от
    корня воркспейса + поле file), ядро создаёт недостающие директории и пишет
    под своим uid — прямой FS-доступ с хоста к data/assets невозможен (проба
    06.11: EACCES, владелец — контейнер). Тот же канал, что undo-журнал cli."""
    _conf()
    # B310: urlopen разрешён только для http/https из UWUWU_SIYUAN_URL.
    if urlparse(URL).scheme not in {"http", "https"}:
        raise RuntimeError(f"putFile: недопустимая схема в UWUWU_SIYUAN_URL: {URL!r}")
    boundary = "----wiki-ops-undo-journal"
    body = b"".join([
        f"--{boundary}\r\n".encode(),
        b'Content-Disposition: form-data; name="path"\r\n\r\n',
        f"{ws_path}\r\n".encode(),
        f"--{boundary}\r\n".encode(),
        b'Content-Disposition: form-data; name="file"; filename="undo.json"\r\n',
        b"Content-Type: application/octet-stream\r\n\r\n",
        payload,
        f"\r\n--{boundary}--\r\n".encode(),
    ])
    req = urllib.request.Request(URL + "/api/file/putFile", data=body,
                                 headers={"Authorization": f"Token {TOKEN}",
                                          "Content-Type": f"multipart/form-data; boundary={boundary}"},
                                 method="POST")
    with urllib.request.urlopen(req, timeout=60) as r:  # nosec B310 — схема http/https проверена выше; URL из UWUWU_SIYUAN_URL ядра
        d = json.loads(r.read())
    if d.get("code") != 0:
        raise RuntimeError(f"putFile: code={d.get('code')} msg={d.get('msg')}")


def _undo_write(operation: str, block_id: str, payload: dict) -> dict | None:
    """Undo-журнал ДО мутации (write-ahead), через kernel putFile — работает с
    любой машины, где доступно ядро. Деградация (ядро отказало) не валит
    операцию — warning + рецепт отката руками. None — журнал не записан."""
    try:
        now = int(time.time())
        name = f"{now}-{operation}-{block_id}.json"
        entry = {"ts": now, "op": operation, "id": block_id, **payload}
        _put_workspace_file(f"{UNDO_DIR}/{name}",
                            json.dumps(entry, ensure_ascii=False, indent=2).encode())
        entry["_name"] = name
        return entry
    except Exception as exc:
        print(f"WARNING: undo-журнал не записан ({exc}); откат — руками: контент из wiki_ops export дока")
        return None


def _undo_patch(journal: dict | None, fields: dict) -> None:
    """Дописать поля (new_ids), известные только после мутации, в записанный журнал."""
    if not journal:
        return
    name = journal.pop("_name", None)
    if not name:
        return
    try:
        journal.update(fields)
        _put_workspace_file(f"{UNDO_DIR}/{name}",
                            json.dumps(journal, ensure_ascii=False, indent=2).encode())
    except Exception as exc:
        print(f"WARNING: undo-журнал не дополнен ({exc})")


def _new_block_ids(resp) -> list[str]:
    """Новые block id из doOperations[].data (data-node-id) — для цепочки операций.
    Форма ответа ядра зависит от эндпоинта: updateBlock — dict, insertBlock —
    список транзакций (проба 06.11)."""
    ops: list[dict] = []
    if isinstance(resp, list):
        ops = [op for tx in resp for op in tx.get("doOperations", [])]
    elif isinstance(resp, dict):
        ops = resp.get("doOperations", [])
    block_ids: list[str] = []
    for op in ops:
        block_ids += re.findall(r'data-node-id="(\d{14}-[a-z0-9]{7})"', op.get("data") or "")
    return block_ids


def _read_payload(filename: str) -> str:
    try:
        payload = Path(filename).read_text()
    except OSError as exc:
        raise RuntimeError(f"payload недоступен: {exc}") from None
    except UnicodeDecodeError as exc:
        raise RuntimeError(f"payload не UTF-8: {exc}") from None
    if not payload.strip():
        raise RuntimeError("пустой payload — блок нельзя обновить/вставить пустотой (снос — block-delete)")
    return payload


def cmd_block_update(a):
    payload = _read_payload(a.file)
    row = _resolved_block(a.blockID.strip())
    chunks = _payload_chunk_count(payload)
    if chunks > 1:
        raise RuntimeError(f"payload содержит {chunks} блока(ов) — updateBlock молча применит только первый, хвост пропадёт (проба 06.11); для нескольких блоков — block-insert-after; многострочный контент одного блока — параграф с переносами или код-блок")
    if row["type"] == "t" and not _payload_has_table_separator(payload):
        raise RuntimeError(f"блок {row['id']} — таблица (type='t'): updateBlock переписывает таблицу ЦЕЛИКОМ — payload без separator-строки шапки схлопнет её до одной строки, code=0 (пробы 06.11/07.11); правка ячейки/строки — в UI или doc-level replace дока; переписать таблицу осознанно — payload = новая таблица целиком (шапка + separator + все строки)")
    journal = _undo_write("update", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "before_md": row["markdown"], "after_md": payload,
    })
    _mutate("/api/block/updateBlock", {"dataType": "markdown", "data": payload, "id": row["id"]})
    print(f"обновлён блок {row['id']} (тип {row['type']})")
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']}")
        print("  откат: block-update этого id с before_md из журнала")


def cmd_block_insert_after(a):
    payload = _read_payload(a.file)
    row = _resolved_block(a.blockID.strip())
    journal = _undo_write("insert-after", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "payload_md": payload,
    })
    resp = _mutate("/api/block/insertBlock", {"dataType": "markdown", "data": payload, "previousID": row["id"]})
    new_ids = _new_block_ids(resp)
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']} (откат: block-delete новых блоков)")
    _undo_patch(journal, {"new_ids": new_ids})
    print(f"вставлено после {row['id']} (тип {row['type']}), новых блоков: {len(new_ids)}")
    for block_id in new_ids:
        print(f"  новый блок: {block_id}")


# --- block-insert-html: живой HTML-блок через dataType="dom" ---

# Префикс маркер-абзаца диаграммы (ADR-052, конвенция cli html_embed).
MARKER_PREFIX = "⁂ diagram:"

# Часовой пояс контейнера ядра (проба 07.11: updated свежей вставки на час
# позади хоста JST) — id/updated генерируются согласованно с его форматом.
KERNEL_TZ = timezone(timedelta(hours=8))

# Пост-чек материализации: SQL-проба новых id (индекс отстаёт от мутаций
# секундами) до фолбэка на .sy.
HTML_POSTCHECK_ATTEMPTS = 3

# Канонический dom-каркас NodeHTMLBlock, снят с живого ядра 07.11 (рецепт
# опыт-дока: короткая markdown-вставка → doOperations[0].data). ZWJ (\u200b)
# в хвостовом span и protyle-attr — часть каркаса ядра, не мусор; data-content
# несёт HTML-эскейп сырого HTML (кавычки обязательны — атрибут в двойных
# кавычках). Маркеры подстановки для str.format, `{`/`}` HTML здесь не несёт.
HTML_BLOCK_DOM_TEMPLATE = (
    '<div data-node-id="{block_id}" data-node-index="1" data-type="NodeHTMLBlock" '
    'class="render-node" updated="{updated}" data-subtype="block">'
    '<div class="protyle-icons">'
    '<span class="ariaLabel protyle-icon protyle-icon--first protyle-action__edit" data-position="4north">'
    '<svg><use xlink:href="#iconEdit"></use></svg></span>'
    '<span class="ariaLabel protyle-icon protyle-action__menu protyle-icon--last" data-position="4north">'
    '<svg><use xlink:href="#iconMore"></use></svg></span></div>'
    '<div><protyle-html data-content="{escaped_html}"></protyle-html>'
    '<span style="position: absolute">\u200b</span></div>'
    '<div class="protyle-attr" contenteditable="false">\u200b</div></div>'
)


def _generate_block_id() -> str:
    """Свежий id в формате ядра: 14 цифр (таймстамп в TZ ядра) + '-' + 7 [a-z0-9]."""
    stamp = datetime.now(KERNEL_TZ).strftime("%Y%m%d%H%M%S")
    tail = "".join(secrets.choice("abcdefghijklmnopqrstuvwxyz0123456789") for _ in range(7))
    return f"{stamp}-{tail}"


def _build_html_block_dom(raw_html: str, block_id: str, updated: str) -> str:
    return HTML_BLOCK_DOM_TEMPLATE.format(
        block_id=block_id, updated=updated,
        escaped_html=html_module.escape(raw_html.strip(), quote=True))


def _sy_tree(doc_id: str) -> dict:
    """Дерево дока из .sy (FS-истина ядра, ADR-025): getFile data/<box><path>.
    RuntimeError с различимыми причинами — транспортный сбой ≠ отсутствие файла
    (ревью плана round-1)."""
    row = _doc_row(doc_id)
    if not row:
        raise RuntimeError(f"док {doc_id} не найден в индексе — .sy-проба невозможна")
    fs_path = _doc_fs_path(row)
    try:
        raw = _get_workspace_file(fs_path)
    except FileNotFoundError:
        raise RuntimeError(f".sy-файл дока отсутствует ({fs_path}) — проверь uwuwu-cli doctor") from None
    except RuntimeError:
        raise RuntimeError(f".sy-файл дока не читается ({fs_path}) — транспорт/ядро; проверь uwuwu-cli doctor") from None
    return json.loads(raw)


def _sy_find_with_next(tree: dict, block_id: str) -> tuple[dict, dict | None] | None:
    """(узел, следующий сиблинг) по ID в дереве .sy (PascalCase Children); None — не найден."""
    def walk(node: dict) -> tuple[dict, dict | None] | None:
        children = node.get("Children") or []
        for index, child in enumerate(children):
            if child.get("ID") == block_id:
                nxt = children[index + 1] if index + 1 < len(children) else None
                return child, nxt
            found = walk(child)
            if found:
                return found
        return None
    return walk(tree)


def _sy_html_block_ids(tree: dict) -> set[str]:
    """Множество ID всех NodeHTMLBlock в дереве .sy."""
    ids: set[str] = set()

    def walk(node: dict) -> None:
        if node.get("Type") == "NodeHTMLBlock":
            ids.add(node.get("ID"))
        for child in node.get("Children") or []:
            walk(child)
    walk(tree)
    return ids


def _resolve_marker_anchor(doc_id: str, marker_name: str) -> tuple[str, str]:
    """Маркер-режим: имя → (blockID маркер-абзаца, root_id дока).

    Уникальность по SQL (порядок детей через индекс невыразим — пробы 07.11):
    0 → отказ с раскладкой маркеров дока; >1 → отказ с раскладкой вхождений.
    Сосед маркера определяется по .sy (FS-истина, паритет cli find_marker_location):
    NodeHTMLBlock-сосед → отказ «диаграмма уже вставлена» с рецептом обновления —
    иначе повторный вызов тихо плодит вторую диаграмму под тем же маркером.
    """
    root_row = _doc_row(doc_id)
    if not root_row:
        raise RuntimeError(f"док {doc_id} не найден — возьми id из wiki_ops find")
    stmt = (f"select id, content from blocks where root_id='{doc_id}' "
            f"and content like '{_sql_str(MARKER_PREFIX)}%' limit 20")  # nosec B608 — docId валидирован DOC_ID_RE, префикс константа
    rows = _sql_rows(stmt)
    markers = [r for r in rows if r.get("content") == MARKER_PREFIX + marker_name]
    known = "; ".join(sorted({r["content"] for r in rows})) or "нет"
    if len(markers) > 1:
        listing = "; ".join(m["id"] for m in markers)
        raise RuntimeError(f"маркер «{MARKER_PREFIX}{marker_name}» не уникален в доке ({listing}) — "
                           "уточни имя или удали лишние маркер-абзацы")
    if not markers:
        raise RuntimeError(f"маркер «{MARKER_PREFIX}{marker_name}» не найден в доке {doc_id}; "
                           f"маркеры в доке: {known}")
    marker_id = markers[0]["id"]
    tree = _sy_tree(doc_id)
    found = _sy_find_with_next(tree, marker_id)
    if not found:
        raise RuntimeError(f"маркер {marker_id} есть в индексе, но не найден в .sy — "
                           "FS и индекс разошлись; проверь uwuwu-cli doctor")
    sibling = found[1]
    if sibling and sibling.get("Type") == "NodeHTMLBlock":
        raise RuntimeError(f"диаграмма по маркеру «{MARKER_PREFIX}{marker_name}» уже вставлена "
                           f"({sibling['ID']}) — обновление: block-delete {sibling['ID']} → "
                           "повтор block-insert-html")
    return marker_id, doc_id


def _verify_html_materialized(new_ids: list[str], doc_id: str) -> str:
    """Пост-чек деградации (два источника вердикта, принцип ADR-025: один лагающий
    источник ≠ истина). Основной — SQL-проба новых id (пробы с паузами: индекс
    отстаёт от мутаций секундами); подтверждённый type≠'html' — немедленный громкий
    отказ. SQL не сошёлся (аномалия/сбой/лаг после всех проб) — фолбэк на .sy
    (FS-истина без лага): новые id как NodeHTMLBlock → успех с примечанием.
    Оба источника молчат → RuntimeError без команды сноса — агент не должен
    удалить реально вставленную диаграмму по ложному негативу."""
    ids_csv = ", ".join(f"'{_sql_str(i)}'" for i in new_ids)
    stmt = f"select id, type from blocks where id in ({ids_csv}) limit {len(new_ids)}"  # nosec B608 — id валидированы форматом (ответ ядра), count явный
    for _ in range(HTML_POSTCHECK_ATTEMPTS):
        try:
            rows = _sql_rows(stmt, attempts=1)
        except Exception:
            break  # индекс не отвечает корректным конвертом — фолбэк на .sy
        by_id = {r["id"]: r.get("type") for r in rows}
        if all(by_id.get(i) == "html" for i in new_ids):
            return "вердикт по SQL"
        degraded = [f"{i} → {by_id[i]}" for i in new_ids if by_id.get(i) not in (None, "html")]
        if degraded:
            raise RuntimeError(
                f"деградация вставки: {', '.join(degraded)} — живой html-блок не материализовался; "
                f"сверь export {doc_id}, откат — block-delete (undo-журнал выше)")
        time.sleep(1)  # id ещё не в индексе — лаг, пробуем снова
    tree = _sy_tree(doc_id)
    html_ids = _sy_html_block_ids(tree)
    if all(i in html_ids for i in new_ids):
        return f"вердикт по .sy (SQL-проба не сошлась за {HTML_POSTCHECK_ATTEMPTS} попыток)"
    raise RuntimeError(
        f"материализация вставки не подтверждена (SQL-проба не сошлась, .sy новых id не содержит) — "
        f"сверь export {doc_id} и SQL вручную до любых действий по откату (undo-журнал выше)")


def cmd_block_insert_html(a):
    raw_html = _read_payload(a.file)
    target = a.target.strip()
    if not DOC_ID_RE.fullmatch(target):
        raise RuntimeError(f"неверный id: {target!r} — ожидается формат SiYuan (14 цифр + '-' + 7 символов)")
    marker_name = a.after_marker
    if marker_name and marker_name.startswith(MARKER_PREFIX):
        marker_name = marker_name[len(MARKER_PREFIX):]
    if marker_name:
        root_probe = _doc_row(target)
        if root_probe:
            doc_id = target
        else:
            row = _resolved_block(target)
            doc_id = row["root_id"]
        anchor, doc_id = _resolve_marker_anchor(doc_id, marker_name.strip())
    else:
        row = _resolved_block(target)
        anchor, doc_id = row["id"], row["root_id"]
    journal = _undo_write("insert-html", anchor, {
        "root_id": doc_id, "hpath": _root_hpath(doc_id),
        "payload_html": raw_html,
    })
    dom = _build_html_block_dom(raw_html, _generate_block_id(),
                                datetime.now(KERNEL_TZ).strftime("%Y%m%d%H%M%S"))
    resp = _mutate("/api/block/insertBlock", {"dataType": "dom", "data": dom, "previousID": anchor})
    new_ids = _new_block_ids(resp)
    if not new_ids:
        raise RuntimeError("ядро не вернул новые id в ответе — вставка могла примениться; "
                           "сверь export/SQL до повтора")
    journal_name = journal["_name"] if journal else None
    _undo_patch(journal, {"new_ids": new_ids})
    verdict = _verify_html_materialized(new_ids, doc_id)
    print(f"вставлен html-блок после {anchor}, новых блоков: {len(new_ids)}")
    for block_id in new_ids:
        print(f"  новый блок: {block_id}")
    print(f"  {verdict}: type='html'")
    if journal_name:
        print(f"  undo-журнал: {UNDO_DIR}/{journal_name} (откат: block-delete новых блоков)")


def cmd_block_delete(a):
    row = _resolved_block(a.blockID.strip())
    if row["type"] == "t":
        raise RuntimeError(f"блок {row['id']} — таблица (type='t'): deleteBlock сносит таблицу ЦЕЛИКОМ, code=0 (проба 07.11); снос таблицы — doc-level replace дока без таблицы (с --base) или в UI; правка/снос строки — в UI")
    children = _block_children_count(row["id"])
    if children:
        raise RuntimeError(f"блок {row['id']} — контейнер: {children} дет.(ей) уйдут вместе с ним тихо (проба 06.11: deleteBlock на list/blockquote сносит поддерево); снос контейнера — конверсией: block-update с одно-блочным payload (проба 06.11: list→параграф) или doc-level replace; посерийный снос item'ов list невозможен — ядро держит пустого ребёнка в каждом item")
    journal = _undo_write("delete", row["id"], {
        "root_id": row["root_id"], "hpath": _root_hpath(row["root_id"]),
        "before_md": row["markdown"],
    })
    _mutate("/api/block/deleteBlock", {"id": row["id"]})
    print(f"снесён блок {row['id']} (тип {row['type']})")
    if journal:
        print(f"  undo-журнал: {UNDO_DIR}/{journal['_name']}")
        print(f"  откат: block-insert-after на предыдущий сиблинг с before_md из журнала (позицию сверь по export {row['root_id']})")


def cmd_orgls(a):
    # Атомарный вывод: буфер печатается только при полном успехе обхода —
    # частичное дерево в stdout при exit 1 читалось бы как полное (задача
    # 20261104072714). Сбой SQL по ноутбуку называет ноутбук и последнюю
    # причину — обход не маскируется под «не найдено».
    lines: list[str] = []
    for nb in notebooks():
        lines.append(f"[{nb['name']}]")
        stmt = f"select hpath from blocks where type='d' and box='{nb['id']}' limit {ORG_LS_LIMIT}"  # nosec B608 — nb.id из kernel API
        try:
            rows = _sql_rows(stmt)
        except RuntimeError as exc:
            raise RuntimeError(
                f"orgls: обход дерева прерван на ноутбуке «{nb['name']}» ({nb['id']}): {exc}"
            ) from None
        for r in sorted(rows, key=lambda x: x["hpath"]):
            depth = r["hpath"].count("/") - 1
            lines.append("  " * (depth + 1) + r["hpath"].rstrip("/").split("/")[-1])
    if lines:
        print("\n".join(lines))


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
    p.add_argument("--base", default=None,
                   help="снапшот свежести: нетронутый вывод wiki_ops export этого дока, снятый до правки")
    p.add_argument("--force", action="store_true",
                   help="осознанный overwrite single-shot: без guard'а свежести, --base не требуется")
    p.set_defaults(fn=cmd_replace)
    p = sub.add_parser("delete")
    p.add_argument("docId")
    p.add_argument("--force", action="store_true")
    p.set_defaults(fn=cmd_delete)
    p = sub.add_parser("block-update")
    p.add_argument("blockID")
    p.add_argument("--md", dest="file", required=True)
    p.set_defaults(fn=cmd_block_update)
    p = sub.add_parser("block-insert-after")
    p.add_argument("blockID")
    p.add_argument("--md", dest="file", required=True)
    p.set_defaults(fn=cmd_block_insert_after)
    p = sub.add_parser("block-insert-html")
    p.add_argument("target", help="docId или blockID: без --after-marker — вставка сразу после блока")
    p.add_argument("--html", dest="file", required=True)
    p.add_argument("--after-marker", default=None, metavar="ИМЯ",
                   help="вставка после маркер-абзаца «⁂ diagram:ИМЯ» в доке (target — docId или blockID того дока)")
    p.set_defaults(fn=cmd_block_insert_html)
    p = sub.add_parser("block-delete")
    p.add_argument("blockID")
    p.set_defaults(fn=cmd_block_delete)
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
