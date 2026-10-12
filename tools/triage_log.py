#!/usr/bin/env python3
"""triage_log — лейбленный корпус мусор-триажа фактов (seed для triage-головы).

Каждая ручная чистка dreaming'а (удалённый мусор + оставленные keep) логируется
сюда. Когда корпус дорастает до ~500 кейсов — на нём можно тренировать
классификационную голову поверх мультиязычного энкодера.

usage:
  triage_log.py file <facts/…> <garbage|keep> <YYYY-MM-DD>   # файл факта на диске
  triage_log.py stdin <garbage|keep> <YYYY-MM-DD> <slug>     # statement на stdin
  triage_log.py stats

Формат facts.triage.json: {sha1_norm: {statement, label, day, slug}}.
sha1_norm — порт 1:1 движка фактов (uwuwu-agent/cli/src/facts.rs):
NFKC → lower → collapse ws → SHA1. statement извлекается портом split_fact
(«---\\n» префикс, разделитель «\\n---\\n», непустое тело) — ключ совместим
с движковым. Повторный add того же statement обновляет label (актуализация).
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import unicodedata
from pathlib import Path

ROOT = Path(os.environ.get("UWUWU_WIKI", Path.home() / ".uwuwu"))
STORE = ROOT / "facts.triage.json"
LABELS = ("garbage", "keep")
STMT_MIN_CHARS = 15  # симметрия MIN движка: пустышки не засоряют корпус


def valid_day(s: str) -> bool:
    """Порт valid_date (facts.rs): форма + месяц 01-12, день 01-31."""
    if len(s) != 10 or not all(
        c == "-" if i in (4, 7) else c.isdigit() for i, c in enumerate(s)
    ):
        return False
    return 1 <= int(s[5:7]) <= 12 and 1 <= int(s[8:10]) <= 31


def sha1_norm(text: str) -> str:
    norm = " ".join(unicodedata.normalize("NFKC", text).lower().split())
    return hashlib.sha1(norm.encode()).hexdigest()


def statement_of(body: str) -> str | None:
    """Порт 1:1 split_fact (facts.rs): None = невалидный файл факта."""
    if not body.startswith("---\n"):
        return None
    rest = body[4:]
    idx = rest.find("\n---\n")
    if idx == -1:
        return None
    stmt = rest[idx + 5 :].strip()
    return stmt or None


def load() -> dict:
    if STORE.exists():
        try:
            return json.loads(STORE.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            raise SystemExit(
                f"facts.triage.json бит (не JSON): {e}\n"
                f"Восстановите из git: git -C {ROOT} checkout -- facts.triage.json"
            ) from e
    return {}


def save(store: dict) -> None:
    tmp = STORE.with_suffix(".tmp")
    tmp.write_text(
        json.dumps(store, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    tmp.replace(STORE)


def add(store: dict, stmt: str | None, label: str, day: str, slug: str) -> str:
    if label not in LABELS:
        raise SystemExit(f"label must be one of {LABELS}, got {label!r}")
    if not valid_day(day):
        raise SystemExit(f"day must be real YYYY-MM-DD, got {day!r}")
    if stmt is None or len(stmt) < STMT_MIN_CHARS:
        preview = (stmt or "")[:60].replace("\n", " ")
        raise SystemExit(
            f"statement пуст/короче {STMT_MIN_CHARS} симв. — в корпус не пишем: {preview!r}"
        )
    key = sha1_norm(stmt)
    store[key] = {"statement": stmt, "label": label, "day": day, "slug": slug}
    return key


def relslug(path: str) -> str:
    p = Path(path)
    if p.is_absolute():
        try:
            p = p.relative_to(ROOT)
        except ValueError:
            pass
    return str(p)


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] == "stats":
        store = load()
        by_label: dict[str, int] = {}
        for rec in store.values():
            by_label[rec["label"]] = by_label.get(rec["label"], 0) + 1
        days = sorted({rec["day"] for rec in store.values()})
        print(
            f"corpus: {len(store)} cases, {by_label}, days: {days[0]}..{days[-1]}"
            if store
            else "corpus: empty"
        )
        return 0

    mode = argv[1]
    store = load()
    if mode == "file" and len(argv) == 5:
        path, label, day = relslug(argv[2]), argv[3], argv[4]
        try:
            body = Path(ROOT / path).read_text(encoding="utf-8")
        except OSError as e:
            raise SystemExit(f"не читается {path}: {e}") from e
        stmt = statement_of(body)
        slug = path
    elif mode == "stdin" and len(argv) == 5:
        label, day, slug = argv[2], argv[3], relslug(argv[4])
        stmt = sys.stdin.read().strip() or None
    else:
        print(__doc__, file=sys.stderr)
        return 2
    key = add(store, stmt, label, day, slug)
    save(store)
    print(f"{key[:12]} -> {label} ({slug})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
