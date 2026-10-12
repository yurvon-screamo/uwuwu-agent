#!/usr/bin/env python3
"""facts_extract — оркестратор фактизации дней opencode-сессий.

Требования окружения: uwuwu-cli в PATH (override: UWUWU_CLI),
ollama с qwen3.5:4b, git-репо вики (UWUWU_WIKI, дефолт ~/.uwuwu),
opencode.db (Linux XDG-путь, override: --db у opencode_audit).
НЕ смешивать UWUWU_DATA_DIR с UWUWU_WIKI (кэш и факты разойдутся по разным корням).

    .venv/bin/python tools/facts_extract.py YYYY-MM-DD [YYYY-MM-DD ...]

Движок фактизации (экстракция → санитайзинг → полный дедуп → запись →
scoped git-коммит) — ЕДИНЫЙ: `uwuwu-cli facts` (cli/src/facts.rs).
Этот скрипт отвечает только за оркестрацию сессий:

  - день → primary-дайджесты (session_view) → `uwuwu-cli facts --stdin`
    с --source opencode/<date>/<sid> и --date <день>;
  - sidecar-атомарность facts.ing.json: marker "messages/dur"; сессия
    помечается ТОЛЬКО после ok (включая trivial/пустой выхлоп);
  - retry/gave-up: bad_jsonl → сессия не помечается; после 3 битых прогонов
    помечается "<marker>#gave-up";
  - ollama_unreachable — фатально (день аборт, сессии не теряются);
  - в конце дня: `uwuwu-cli facts --sync` (sidecar-файлы коммитит движок).

Контракты факта/дедупа/промпта см. cli/src/facts.rs (одна точка правды).
"""

import json
import os
import subprocess
import sys
from datetime import date as _date
from datetime import timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from session_view import export_day, to_doc

WIKI = Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))
SIDECAR = WIKI / "facts.ing.json"
RETRIES = WIKI / "facts.retries.json"
TODAY = _date.today().isoformat()

CLI = os.environ.get("UWUWU_CLI", "uwuwu-cli")
# worst-case движка: экстракция 600с + вердикт-пачки (до 2×600с) + буфер
CLI_TIMEOUT_S = 2400
MAX_EXTRACT_RETRIES = 3


def facts_engine(text: str, source: str, date: str) -> dict:
    """Вызов единого движка (uwuwu-cli facts). stdout — JSON, warn'ы — stderr."""
    proc = subprocess.run(
        [CLI, "facts", "--stdin", "--source", source, "--date", date],
        input=text,
        capture_output=True,
        text=True,
        timeout=CLI_TIMEOUT_S,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"uwuwu-cli facts failed: {proc.stderr.strip()[:200]}")
    return json.loads(proc.stdout)


def load_json(path: Path, default):
    if path.exists():
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except Exception:  # noqa: BLE001 — битый json = дефолт
            return default
    return default


def save_json(path: Path, data) -> None:
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    tmp.replace(path)


def process_digest(doc: dict, sidecar: dict, retries: dict, stats: dict) -> None:
    """Оркестрация одного дайджеста через движок."""
    uri, marker = doc["uri"], doc["marker"]
    marked = sidecar.get(uri)
    if marked == marker or marked == f"{marker}#gave-up":
        stats["skipped"] += 1
        return
    stats["sessions"] += 1

    out = facts_engine(doc["text"], uri, doc.get("date") or TODAY)

    if not out.get("ok"):
        err = out.get("error")
        if err == "ollama_unreachable":
            raise RuntimeError("ollama_unreachable")  # фатально: день аборт
        if err == "bad_jsonl":
            # Частично битый выхлоп — retry-able: сессия НЕ помечается,
            # перепрогон подхватит; после MAX_EXTRACT_RETRIES — gave-up.
            retries[uri] = retries.get(uri, 0) + 1
            if retries[uri] >= MAX_EXTRACT_RETRIES:
                sidecar[uri] = f"{marker}#gave-up"
                print(
                    f"    warn: {uri}: {retries[uri]} битых прогонов — помечена "
                    "gave-up, пропускается",
                    file=sys.stderr,
                )
            return
        # Иные ошибки движка (IO, lock-таймаут, bad_date) — фатальны для дня:
        # raise прерывает день, сессия НЕ помечается и не gave-up (тихая потеря
        # исключена: следующий прогон переиграет).
        raise RuntimeError(f"engine error: {err}")

    # ok (включая trivial и пустой выхлоп): маркер сессии ставится.
    stats["facts"] += len(out.get("new") or [])
    stats["dup"] += len(out.get("dup") or [])
    stats["conflict"] += len(out.get("conflict") or [])
    if out.get("trivial"):
        stats["skipped"] += 1
    sidecar[uri] = marker
    retries.pop(uri, None)


GROWN_CHECK_DAYS = 3  # авто-проверка доросших сессий: свежие дни перед диапазоном


def process_day(date: str, sidecar: dict, retries: dict, auto: bool = False) -> dict:
    tag = "[grown-check] " if auto else ""
    stats = {
        "sessions": 0,
        "skipped": 0,
        "facts": 0,
        "dup": 0,
        "conflict": 0,
        "upgrown": 0,
    }
    day = export_day(date)
    primaries = day.get("primaries", [])
    if not primaries and not day.get("upgrown"):
        print(f"{tag}{date}: нет primary-сессий")
        return stats
    for session in primaries:
        doc = to_doc(date, session)
        doc["date"] = date
        process_digest(doc, sidecar, retries, stats)
    # Доросшие сессии: созданы раньше, активны в этот день. Обрабатываются
    # с РОДНЫМ днём создания — uri/marker бьются с существующими sidecar-ключами
    # (STALE-маркер → переобработка только дельты фактов, дедуп ловит старьё).
    for session in day.get("upgrown", []):
        native = session.get("native_day") or date
        doc = to_doc(native, session)
        doc["date"] = native
        stats["upgrown"] += 1
        process_digest(doc, sidecar, retries, stats)
    save_json(SIDECAR, sidecar)
    save_json(RETRIES, retries)
    # Sidecar-файлы коммитит движок (Rust — единственный коммитер)
    sync = subprocess.run([CLI, "facts", "--sync"], capture_output=True, check=False)
    if sync.returncode != 0 or sync.stderr.strip():
        print(f"    warn: facts --sync: {sync.stderr.strip()[:160]}", file=sys.stderr)
    print(
        f"{tag}{date}: sessions={stats['sessions']} upgrown={stats['upgrown']} skipped={stats['skipped']} "
        f"new={stats['facts']} dup={stats['dup']} conflict={stats['conflict']}"
    )
    return stats


def main() -> None:
    if len(sys.argv) < 2:
        print("usage: facts_extract.py YYYY-MM-DD [YYYY-MM-DD ...]", file=sys.stderr)
        sys.exit(1)
    try:
        requested = sorted({_date.fromisoformat(d).isoformat() for d in sys.argv[1:]})
    except ValueError as exc:
        print(
            f"невалидная дата в аргументах (ожидаются YYYY-MM-DD): {exc}",
            file=sys.stderr,
        )
        sys.exit(2)
    dates = requested
    sidecar = load_json(SIDECAR, {})
    retries = load_json(RETRIES, {})
    # Хвост авто-проверки: сессии могли дорости ПОСЛЕ маркера утреннего прогона
    # (живые долгие сессии). process_day скипает немаркированные по sidecar-маркеру
    # (STALE-сравнение), поэтому проверка дешёвая: LLM получают только дельту.
    # NB: хвост считается ПЕРЕД dates[0] — при глубоком backfill он уходит в ещё
    # более старые дни и бесполезен (но дёшев: давно замаркировано → скип);
    # доросших backfill-диапазона ловят сами его дни через upgrown-EXISTS.
    oldest = _date.fromisoformat(dates[0])
    auto_dates = sorted(
        (oldest - timedelta(days=back)).isoformat()
        for back in range(1, GROWN_CHECK_DAYS + 1)
        if (oldest - timedelta(days=back)).isoformat() < TODAY
        and (oldest - timedelta(days=back)).isoformat() not in dates
    )
    total = 0
    grown_total = 0
    for d in auto_dates:
        try:
            st = process_day(d, sidecar, retries, auto=True)
            total += st["facts"]
            grown_total += st["facts"]
        except Exception as exc:  # noqa: BLE001 — день падает, остальные идут
            print(f"[grown-check] {d}: FAILED ({exc})", file=sys.stderr)
    for date in dates:
        try:
            st = process_day(date, sidecar, retries)
            total += st["facts"]
        except Exception as exc:  # noqa: BLE001 — день падает, остальные идут
            print(f"{date}: FAILED ({exc})", file=sys.stderr)
    grown_note = f" (+{grown_total} grown-check)" if grown_total else ""
    print(f"итого новых фактов: {total}{grown_note}")


if __name__ == "__main__":
    main()
