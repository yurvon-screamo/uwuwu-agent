#!/usr/bin/env python3
"""session_view — дайджест-представление сессий opencode за дату (без memvid).

Копия логики daily_ingest.export_day/to_doc, изолированная от memvid-SDK
(facts_extract не должен зависеть от замороженного стека памяти).

ПРИНЯТЫЙ РИСК (code review): дрейф с opencode_audit.py при изменении его
формата — синхронизировать вручную. daily_ingest.py заморожен и не трогается.
"""

import json
import subprocess
import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
AUDIT = TOOLS / "opencode_audit.py"

# Бюджет дайджеста (как в daily_ingest)
USER_MSGS, USER_CHARS = 3, 1000
ASST_MSGS, ASST_CHARS = 3, 1500
KID_TITLES = 8

PROJECT_ALIASES = {
    "System32": "yurvon",
    "Downloads": "yurvon",
    "unknown": "yurvon",
    "origa-grammar-v2": "origa",
}


def export_day(date: str) -> dict:
    """Экспорт дня через opencode_audit (JSON, чистый stdlib)."""
    out = subprocess.run(
        [sys.executable, str(AUDIT), "--compact", "day", date],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(out.stdout)


def _clip(text: str | None, limit: int) -> str:
    text = (text or "").strip()
    return text if len(text) <= limit else text[:limit] + "…"


def _section(items: list[str], limit_n: int, limit_chars: int) -> str:
    picked = [f"{i}. {_clip(t, limit_chars)}" for i, t in enumerate(items[:limit_n], 1)]
    skipped = len(items) - min(len(items), limit_n)
    if skipped > 0:
        picked.append(f"…(ещё {skipped})")
    return "\n".join(picked)


def _kids_block(session: dict) -> str:
    lines: list[str] = []
    kids = session.get("kids") or {}
    for agent, info in sorted(kids.items()):
        titles = [t for t in (info.get("titles") or []) if t][: KID_TITLES // 2]
        extra = info.get("count", 0) - len(titles)
        suffix = f" (ещё {extra})" if extra > 0 else ""
        for t in titles:
            lines.append(f"- [{agent}] {t}")
        if not titles and info.get("count"):
            lines.append(f"- [{agent}] ×{info['count']}{suffix}")
    return "\n".join(lines)


def to_doc(date: str, session: dict) -> dict:
    """Сессия → компактный дайджест-документ (текст для LLM-экстракции)."""
    dur_min = session.get("dur_ms", 0) / 60000
    summary = session.get("summary") or {}
    header = [
        f"# {session.get('title') or session['id']}",
        "",
        f"- session: {session['id']}",
        f"- date: {date}, dur: {dur_min:.0f} min, messages: {session.get('messages_total', 0)}",
        f"- agent: {session.get('agent')}, directory: {session.get('directory')}",
    ]
    if any(summary.get(k) for k in ("additions", "deletions", "files")):
        header.append(
            f"- diff: +{summary.get('additions') or 0}/-{summary.get('deletions') or 0}"
            f" in {summary.get('files') or 0} files"
        )

    parts = ["\n".join(header)]
    if session.get("user_msgs"):
        parts.append(
            "## Запросы пользователя\n"
            + _section(session["user_msgs"], USER_MSGS, USER_CHARS)
        )
    if session.get("asst_texts"):
        parts.append(
            "## Итог агента\n" + _section(session["asst_texts"], ASST_MSGS, ASST_CHARS)
        )
    kids = _kids_block(session)
    if kids:
        parts.append("## Subagents\n" + kids)

    return {
        "title": session.get("title") or session["id"],
        "uri": f"opencode/{date}/{session['id']}",
        "text": "\n\n".join(parts),
        # Маркер свежести (как в daily_ingest): изменился → сессия доросла
        "marker": f"{session.get('messages_total', 0)}/{int(session.get('dur_ms') or 0) // 1000}",
    }
