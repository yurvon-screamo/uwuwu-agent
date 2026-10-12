#!/usr/bin/env python3
"""opencode audit — data layer для аудита сессий opencode.

Скрипт только читает opencode.db (SELECT) и печатает JSON в stdout.
LLM-фасовка (enrichment experience/access) — задача
dreaming-агента, этот скрипт её не делает.

Схема opencode v2: сессии живут в `session_v2` (надмножество легаси-таблицы
`session`, которая заморожена); `message`/`part` не изменились; таблица `event`
в v2 почти не пишется — forensic-события (tool fails/retries) пустые.

Subcommands:
  overview [--days N | --since YYYY-MM-DD] [--until YYYY-MM-DD]
      Общая статистика за период: сессии по агентам/дням, primary/subagent соотношение.
  day YYYY-MM-DD
      Детальный dump одного дня: primary-сессии с user/asst/shell/tool_uses/
      timeline/kids + subagent summary. Используется audit-LLM для фасовки
      опыта за день.
  range YYYY-MM-DD YYYY-MM-DD
      Детальный dump диапазона дней (включительно). Для массового прогона.
  session SESSION_ID
      Forensic-дамп одной сессии: meta + parent/children + timeline +
      user/asst/shell/tool_uses + events.

Все команды печатают JSON в stdout (pretty-print по умолчанию,
--compact для машинного).
Config: --db PATH (default: стандартный opencode.db),
--wiki PATH (default: ../ от скрипта).
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path

# Defaults (Linux/XDG; на других ОС задавайте пути явно: --db, UWUWU_WIKI)
DEFAULT_DB = Path.home() / ".local" / "share" / "opencode" / "opencode.db"
DEFAULT_WIKI = Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))

# opencode v2: живая таблица сессий (легаси `session` заморожена на миграции)
SESSION_TABLE = "session_v2"

# Truncation defaults (per-item)
LIMIT_USER_MSGS = 5
LIMIT_ASST_TEXTS = 3
LIMIT_SHELL_CMDS = 30
LIMIT_KID_TITLES = 8
TEXT_TRUNCATE = 1500
LIMIT_TIMELINE = 150
LIMIT_TOOL_USE_SAMPLES = 40
SHELL_OUTPUT_PREVIEW = 500
LIMIT_REASONING = 3
REASONING_TRUNCATE = 600


# ---------- DB helpers ----------


def connect(db_path: Path) -> sqlite3.Connection:
    if not db_path.exists():
        sys.exit(f"ERROR: opencode.db not found at {db_path}")
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    return con


def day_bounds(date_str: str) -> tuple[int, int]:
    """UTC day boundaries as unix-ms."""
    dt = datetime.strptime(date_str, "%Y-%m-%d").replace(tzinfo=UTC)
    start = int(dt.timestamp() * 1000)
    return start, start + 86_400_000


def parse_date(s: str) -> str:
    """Validate YYYY-MM-DD."""
    datetime.strptime(s, "%Y-%m-%d").replace(tzinfo=UTC)
    return s


# ---------- Queries ----------


def q_sessions_in_range(
    con: sqlite3.Connection, start_ms: int, end_ms: int
) -> list[dict]:
    cur = con.execute(
        f"""SELECT id, slug, title, agent, directory, parent_id,
                  summary_additions, summary_deletions, summary_files,
                  time_created, time_updated
           FROM {SESSION_TABLE}
           WHERE time_created >= ? AND time_created < ?
           ORDER BY time_created""",
        (start_ms, end_ms),
    )
    return [dict(r) for r in cur.fetchall()]


def _parse_json_dict(raw: str | None) -> dict:
    """Parse a JSON string into a dict; tolerant of None/garbage."""
    if not raw:
        return {}
    try:
        d = json.loads(raw)
    except json.JSONDecodeError:
        return {}
    return d if isinstance(d, dict) else {}


def _as_dict(value: object) -> dict:
    return value if isinstance(value, dict) else {}


def _iter_tool_parts(content: list[dict]):
    """Yield part dicts of type 'tool' from assistant messages, chronological."""
    for m in content:
        if m["role"] != "assistant":
            continue
        for p in m["parts"]:
            if isinstance(p, dict) and p.get("type") == "tool":
                yield p


def load_session_content(con: sqlite3.Connection, sid: str) -> list[dict]:
    """Load messages + parts for a session into an ordered list of message-buckets.

    opencode v2: контент в `session_message` (type=user/assistant/..., data JSON:
    user → data.text, assistant → data.content[] с parts; tool-part хранит имя в
    `name`, вывод в `state.content`). Легаси-сессии (до v2) — в `message`+`part`;
    берётся первое хранилище, где есть строки.

    Returns list (chronological) of:
        {"time": int, "role": "user"|"assistant"|..., "meta": <message.data dict>,
         "parts": [<part dict in legacy shape>, ...]}
    """
    buckets = _load_session_message(con, sid)  # v2 store
    if buckets:
        return buckets
    return _load_message_part(con, sid)  # legacy store


def _normalize_v2_tool(p: dict) -> dict:
    """v2 tool-part → legacy shape: name→tool, state.content[]→state.output."""
    if p.get("type") != "tool":
        return p
    state = dict(_as_dict(p.get("state")))
    output = state.get("output")
    if not output:
        content = state.get("content")
        if isinstance(content, list):
            output = "\n".join(
                c.get("text", "") for c in content if isinstance(c, dict)
            )
    state["output"] = output
    return {"type": "tool", "tool": p.get("name") or p.get("tool"), "state": state}


def _load_session_message(con: sqlite3.Connection, sid: str) -> list[dict]:
    """v2 store: rows from `session_message` normalized to legacy buckets."""
    cur = con.execute(
        """SELECT type, time_created, data FROM session_message
           WHERE session_id = ?
           ORDER BY seq""",
        (sid,),
    )
    out: list[dict] = []
    for r in cur.fetchall():
        d = _parse_json_dict(r["data"])
        if not d:
            continue
        role = r["type"]
        time_json = _as_dict(d.get("time"))
        created = time_json.get("created") or r["time_created"]
        if role == "user":
            parts = [{"type": "text", "text": d["text"]}] if d.get("text") else []
        elif role == "assistant":
            parts = [
                _normalize_v2_tool(p)
                for p in d.get("content") or []
                if isinstance(p, dict)
            ]
        else:  # compaction, synthetic, ... — сохранить как есть
            parts = [p for p in (d.get("content") or []) if isinstance(p, dict)]
        out.append({"time": created, "role": role, "meta": d, "parts": parts})
    return out


def _load_message_part(con: sqlite3.Connection, sid: str) -> list[dict]:
    """Legacy store: `message`+`part` tables (pre-v2 sessions)."""
    cur = con.execute(
        """SELECT m.id AS mid, m.time_created AS mtime, m.data AS mdata,
                  p.time_created AS ptime, p.data AS pdata
           FROM message m
           LEFT JOIN part p ON p.message_id = m.id
           WHERE m.session_id = ?
           ORDER BY m.time_created, p.time_created""",
        (sid,),
    )
    order: list[str] = []
    bucket: dict[str, dict] = {}
    for r in cur.fetchall():
        mid = r["mid"]
        if mid not in bucket:
            md = _parse_json_dict(r["mdata"])
            bucket[mid] = {
                "time": r["mtime"],
                "role": md.get("role") or "?",
                "meta": md,
                "parts": [],
            }
            order.append(mid)
        pd = _parse_json_dict(r["pdata"])
        if pd:
            bucket[mid]["parts"].append(pd)
    return [bucket[mid] for mid in order]


def _first_text(parts: list[dict]) -> str:
    for p in parts:
        if isinstance(p, dict) and p.get("type") == "text" and p.get("text"):
            return p["text"]
    return ""


def _assistant_preview(parts: list[dict]) -> str:
    for p in parts:
        if not isinstance(p, dict):
            continue
        pt = p.get("type")
        if pt == "text" and p.get("text"):
            return p["text"][:200]
        if pt == "tool":
            return f"[tool] {p.get('tool') or '?'}"
        if pt == "reasoning":
            return "[reasoning]"
    return ""


def _message_preview(role: str, parts: list[dict]) -> str:
    if role == "user":
        return _first_text(parts)[:200]
    if role == "assistant":
        return _assistant_preview(parts)
    return ""


def extract_user_msgs(content: list[dict], limit: int, truncate: int) -> list[str]:
    out: list[str] = []
    for m in content:
        if m["role"] != "user":
            continue
        t = _first_text(m["parts"])
        if t:
            out.append(t[:truncate])
            if len(out) >= limit:
                break
    return out


def extract_asst_texts(content: list[dict], limit: int, truncate: int) -> list[str]:
    """Last N assistant text messages (most informative for outcomes)."""
    out: list[str] = []
    for m in reversed(content):
        if m["role"] != "assistant":
            continue
        t = _first_text(m["parts"])
        if t:
            out.append(t[:truncate])
            if len(out) >= limit:
                break
    return out


def extract_reasoning(content: list[dict], limit: int, truncate: int) -> list[str]:
    """First N reasoning snippets — shows the agent's train of thought."""
    out: list[str] = []
    for m in content:
        if m["role"] != "assistant":
            continue
        for p in m["parts"]:
            if isinstance(p, dict) and p.get("type") == "reasoning" and p.get("text"):
                out.append(p["text"][:truncate])
                if len(out) >= limit:
                    return out
    return out


_TOOL_INPUT_KEYS = (
    "command",
    "filePath",
    "path",
    "pattern",
    "query",
    "url",
    "image_path",
    "oldString",
    "replaceAll",
    "content",
)


def _tool_input_key(inp: dict) -> str:
    for k in _TOOL_INPUT_KEYS:
        if k in inp:
            return f"{k}={str(inp[k])[:120]}"
    return ""


def extract_tool_uses(content: list[dict], sample_limit: int) -> dict:
    """Aggregate tool-call counts (from part.type='tool') + first-seen input samples.

    part.type='tool' carries state.input/state.output, so this reveals what the agent
    ACTUALLY did (read/write/edit/grep/bash/...), not just what it narrated.
    """
    counts: dict[str, int] = {}
    samples: list[dict] = []
    for p in _iter_tool_parts(content):
        tool = p.get("tool") or "(unknown)"
        counts[tool] = counts.get(tool, 0) + 1
        if len(samples) >= sample_limit:
            continue
        state = _as_dict(p.get("state"))
        inp = _as_dict(state.get("input"))
        samples.append(
            {"tool": tool, "status": state.get("status"), "input": _tool_input_key(inp)}
        )
    return {"counts": counts, "samples": samples}


def _shell_output(state: dict) -> str:
    output = state.get("output") or ""
    if not output:
        meta = state.get("metadata")
        if isinstance(meta, dict):
            output = meta.get("output") or ""
    return output if isinstance(output, str) else ""


def extract_shell(content: list[dict], limit: int, output_preview: int) -> list[dict]:
    """Shell invocations (tool='bash') with output preview. Dedup by command."""
    seen: set[str] = set()
    out: list[dict] = []
    for p in _iter_tool_parts(content):
        cmd = _bash_command(p)
        if not cmd or cmd in seen:
            continue
        seen.add(cmd)
        state = _as_dict(p.get("state"))
        output = _shell_output(state)
        out.append(
            {
                "command": cmd[:300],
                "output_preview": output[:output_preview],
                "output_len": len(output),
                "status": state.get("status"),
            }
        )
        if len(out) >= limit:
            return out
    return out


def _bash_command(part: dict) -> str:
    if part.get("tool") != "bash":
        return ""
    state = _as_dict(part.get("state"))
    inp = _as_dict(state.get("input"))
    return inp.get("command") or ""


def extract_timeline(content: list[dict], limit: int) -> list[dict]:
    """Chronological message spine with short preview per message."""
    out: list[dict] = []
    for m in content:
        out.append(
            {
                "role": m["role"],
                "time": m["time"],
                "preview": _message_preview(m["role"], m["parts"]),
            }
        )
        if len(out) >= limit:
            break
    return out


_NO_AGENT = "(none)"
_NOISE_TITLE_PREFIXES = ("New session", "Intro message")


def _clean_title(title: str | None) -> str:
    title = title or ""
    if title.startswith(_NOISE_TITLE_PREFIXES):
        return ""
    return title


def q_kids_summary(
    con: sqlite3.Connection, parent_id: str, limit_titles: int
) -> dict[str, dict]:
    cur = con.execute(
        f"""SELECT agent, title FROM {SESSION_TABLE}
           WHERE parent_id = ? ORDER BY time_created""",
        (parent_id,),
    )
    summary: dict[str, dict] = {}
    for r in cur.fetchall():
        agent = r["agent"] or _NO_AGENT
        title = _clean_title(r["title"])
        summary.setdefault(agent, {"count": 0, "titles": []})
        summary[agent]["count"] += 1
        if title and len(summary[agent]["titles"]) < limit_titles:
            summary[agent]["titles"].append(title)
    return summary


def q_kids_list(con: sqlite3.Connection, parent_id: str) -> list[dict]:
    """Flat list of child sessions with metrics (no text).

    For spotting which subagent did real work.
    """
    cur = con.execute(
        f"""SELECT id, slug, title, agent, directory,
                  summary_additions, summary_deletions, summary_files,
                  time_created, time_updated
           FROM {SESSION_TABLE} WHERE parent_id = ?
           ORDER BY time_created""",
        (parent_id,),
    )
    out: list[dict] = []
    for r in cur.fetchall():
        s = dict(r)
        out.append(
            {
                "id": s["id"],
                "slug": s["slug"],
                "agent": s["agent"],
                "title": _clean_title(s["title"]),
                "dur_ms": s["time_updated"] - s["time_created"],
                "summary": {
                    "additions": s["summary_additions"],
                    "deletions": s["summary_deletions"],
                    "files": s["summary_files"],
                },
            }
        )
    return out


def q_session_events(con: sqlite3.Connection, sid: str) -> dict:
    """Forensic: tool calls, fails, retries.

    NOTE (opencode v2): таблица `event` больше не пишет session.next.*-события —
    этот блок стабильно пустой; tool-активность доступна через part.type='tool'
    (extract_tool_uses). Оставлено для старых дампов.
    """
    tool_calls: dict[str, int] = {}
    tool_fails: list[dict] = []
    retries: list[dict] = []
    cur = con.execute(
        """SELECT type, json_extract(data, '$.tool') AS tool,
                  json_extract(data, '$.error.message') AS err,
                  json_extract(data, '$.attempt') AS attempt
           FROM event
           WHERE aggregate_id = ? AND type IN (
             'session.next.tool.called.1',
             'session.next.tool.failed.1',
             'session.next.retried.1')
           ORDER BY seq""",
        (sid,),
    )
    for r in cur.fetchall():
        if r["type"] == "session.next.tool.called.1":
            tool = r["tool"] or "(unknown)"
            tool_calls[tool] = tool_calls.get(tool, 0) + 1
        elif r["type"] == "session.next.tool.failed.1":
            tool_fails.append({"tool": r["tool"], "error": r["err"]})
        elif r["type"] == "session.next.retried.1":
            retries.append({"attempt": r["attempt"], "error": r["err"]})
    return {"tool_calls": tool_calls, "tool_fails": tool_fails, "retries": retries}


def build_primary_detail(
    con: sqlite3.Connection, s: dict, args: argparse.Namespace
) -> dict:
    sid = s["id"]
    kids_grouped = q_kids_summary(con, sid, args.limit_kid_titles)
    kids_list = q_kids_list(con, sid)
    content = load_session_content(con, sid)
    return {
        "id": sid,
        "slug": s["slug"],
        "title": s["title"],
        "agent": s["agent"],
        "directory": s["directory"],
        "project": Path(s["directory"]).name if s["directory"] else None,
        "day": datetime.fromtimestamp(s["time_created"] / 1000, tz=UTC).strftime(
            "%Y-%m-%d"
        ),
        "time_created": s["time_created"],
        "dur_ms": s["time_updated"] - s["time_created"],
        "summary": {
            "additions": s["summary_additions"],
            "deletions": s["summary_deletions"],
            "files": s["summary_files"],
        },
        "messages_total": len(content),
        "timeline": extract_timeline(content, args.limit_timeline),
        "user_msgs": extract_user_msgs(
            content, args.limit_user_msgs, args.text_truncate
        ),
        "asst_texts": extract_asst_texts(
            content, args.limit_asst_texts, args.text_truncate
        ),
        "reasoning": extract_reasoning(
            content, args.limit_reasoning, args.reasoning_truncate
        ),
        "tool_uses": extract_tool_uses(content, args.limit_tool_use_samples),
        "shell": extract_shell(
            content, args.limit_shell_cmds, args.shell_output_preview
        ),
        "kids": kids_grouped,
        "kids_list": kids_list,
        "kids_total": len(kids_list),
    }


# ---------- Subcommands ----------


def cmd_overview(args: argparse.Namespace) -> dict:
    con = connect(args.db)
    if args.since:
        start_dt = datetime.strptime(args.since, "%Y-%m-%d").replace(tzinfo=UTC)
        start_ms = int(start_dt.timestamp() * 1000)
    else:
        start_ms = int(
            (datetime.now(UTC) - timedelta(days=args.days)).timestamp() * 1000
        )
    if args.until:
        end_dt = datetime.strptime(args.until, "%Y-%m-%d").replace(tzinfo=UTC)
        end_ms = int(end_dt.timestamp() * 1000)
    else:
        end_ms = int(datetime.now(UTC).timestamp() * 1000)

    sessions = q_sessions_in_range(con, start_ms, end_ms)

    by_day: dict[str, dict] = {}
    by_agent: dict[str, dict] = {}
    for s in sessions:
        day = datetime.fromtimestamp(s["time_created"] / 1000, tz=UTC).strftime(
            "%Y-%m-%d"
        )
        d = by_day.setdefault(day, {"primary": 0, "subagent": 0, "total": 0})
        d["total"] += 1
        if s["parent_id"]:
            d["subagent"] += 1
        else:
            d["primary"] += 1
        a = by_agent.setdefault(
            s["agent"] or _NO_AGENT, {"primary": 0, "subagent": 0, "total": 0}
        )
        a["total"] += 1
        if s["parent_id"]:
            a["subagent"] += 1
        else:
            a["primary"] += 1

    return {
        "period": {
            "since": datetime.fromtimestamp(start_ms / 1000, tz=UTC).strftime(
                "%Y-%m-%d"
            ),
            "until": datetime.fromtimestamp(end_ms / 1000, tz=UTC).strftime("%Y-%m-%d"),
        },
        "totals": {
            "sessions": len(sessions),
            "primary": sum(1 for s in sessions if not s["parent_id"]),
            "subagent": sum(1 for s in sessions if s["parent_id"]),
        },
        "by_day": dict(sorted(by_day.items())),
        "by_agent": dict(sorted(by_agent.items(), key=lambda x: -x[1]["total"])),
    }


def cmd_day(args: argparse.Namespace) -> dict:
    con = connect(args.db)
    start_ms, end_ms = day_bounds(args.date)
    sessions = q_sessions_in_range(con, start_ms, end_ms)

    primaries = [s for s in sessions if not s["parent_id"]]
    subagents = [s for s in sessions if s["parent_id"]]

    # Group subagents by parent (days with no primaries — recovered from kids)
    subagents_by_parent: dict[str, list[dict]] = {}
    parent_meta: dict[str, dict] = {}
    for s in subagents:
        pid = s["parent_id"]
        subagents_by_parent.setdefault(pid, []).append(s)
        if pid not in parent_meta:
            # Parent may be outside this day (long-running session started earlier)
            cur = con.execute(
                f"SELECT slug, title, agent, directory FROM {SESSION_TABLE} WHERE id = ?",
                (pid,),
            )
            row = cur.fetchone()
            if row:
                parent_meta[pid] = dict(row)

    # Orphan subagents = parent not in primaries list
    primary_ids = {p["id"] for p in primaries}
    orphan_parents = {
        pid: subs for pid, subs in subagents_by_parent.items() if pid not in primary_ids
    }

    primaries_detail = [build_primary_detail(con, p, args) for p in primaries]

    orphan_summary = []
    for pid, kids in orphan_parents.items():
        meta = parent_meta.get(pid, {})
        agent_groups: dict[str, list[str]] = {}
        for k in kids:
            agent_groups.setdefault(k["agent"] or _NO_AGENT, []).append(
                k["title"] or ""
            )
        orphan_summary.append(
            {
                "parent_id": pid,
                "parent_slug": meta.get("slug"),
                "parent_title": meta.get("title"),
                "parent_agent": meta.get("agent"),
                "parent_directory": meta.get("directory"),
                "kids_by_agent": {
                    a: {
                        "count": len(ts),
                        "titles": [t for t in ts if not t.startswith("New session")][
                            : args.limit_kid_titles
                        ],
                    }
                    for a, ts in agent_groups.items()
                },
            }
        )

    # Upgrown: созданы раньше, АКТИВНЫ в этот день. Критерий активности —
    # наличие сообщений в дне (EXISTS по session_message), а НЕ time_updated:
    # time_updated указывает лишь на ПОСЛЕДНИЙ день сессии, из-за чего
    # многодневная сессия была невидима в промежуточных днях (дыра: сообщения
    # 24–27.09 не фактизировались, пока сессия не завершится). Доросшие
    # сессии принадлежат дню создания (time_created). Каждой — native_day.
    upgrown_rows = con.execute(
        f"""SELECT s.id, s.slug, s.title, s.agent, s.directory, s.parent_id,
                   s.summary_additions, s.summary_deletions, s.summary_files,
                   s.time_created, s.time_updated
            FROM {SESSION_TABLE} s
            WHERE s.time_created < ?
              AND s.parent_id IS NULL
              AND EXISTS (SELECT 1 FROM session_message m
                          WHERE m.session_id = s.id
                            AND m.time_created >= ? AND m.time_created < ?
                            AND m.type IN ('user', 'assistant', 'shell'))
            ORDER BY s.time_created""",
        (start_ms, start_ms, end_ms),
    ).fetchall()
    upgrown_detail = []
    for s in upgrown_rows:
        d = build_primary_detail(con, dict(s), args)
        d["native_day"] = datetime.fromtimestamp(
            s["time_created"] / 1000, tz=UTC
        ).strftime("%Y-%m-%d")
        upgrown_detail.append(d)

    return {
        "day": args.date,
        "stats": {
            "primary": len(primaries),
            "subagent": len(subagents),
            "distinct_parents": len(subagents_by_parent),
            "orphan_parents": len(orphan_parents),
            "upgrown": len(upgrown_detail),
        },
        "primaries": primaries_detail,
        "upgrown": upgrown_detail,  # доросшие: родной день в native_day
        "orphan_parents": orphan_summary,  # subagents whose primary is outside this day
    }


def cmd_range(args: argparse.Namespace) -> dict:
    """Iterate days in range, run cmd_day for each."""
    start_dt = datetime.strptime(args.start, "%Y-%m-%d").replace(tzinfo=UTC)
    end_dt = datetime.strptime(args.end, "%Y-%m-%d").replace(tzinfo=UTC)
    days: list[str] = []
    cur_dt = start_dt
    while cur_dt <= end_dt:
        days.append(cur_dt.strftime("%Y-%m-%d"))
        cur_dt += timedelta(days=1)

    # Re-use cmd_day per day, share connection
    con = connect(args.db)
    out_days = []
    for d in days:
        ns = argparse.Namespace(
            date=d,
            db=args.db,
            wiki=args.wiki,
            limit_user_msgs=args.limit_user_msgs,
            limit_asst_texts=args.limit_asst_texts,
            limit_shell_cmds=args.limit_shell_cmds,
            limit_kid_titles=args.limit_kid_titles,
            text_truncate=args.text_truncate,
            limit_timeline=args.limit_timeline,
            limit_tool_use_samples=args.limit_tool_use_samples,
            shell_output_preview=args.shell_output_preview,
            limit_reasoning=args.limit_reasoning,
            reasoning_truncate=args.reasoning_truncate,
        )
        # Inline cmd_day to share connection
        start_ms, end_ms = day_bounds(d)
        sessions = q_sessions_in_range(con, start_ms, end_ms)
        if not sessions:
            out_days.append(
                {
                    "day": d,
                    "stats": {
                        "primary": 0,
                        "subagent": 0,
                        "distinct_parents": 0,
                        "orphan_parents": 0,
                        "empty": True,
                    },
                    "primaries": [],
                    "orphan_parents": [],
                }
            )
            continue
        out_days.append(cmd_day(ns))
    return {"range": {"start": args.start, "end": args.end}, "days": out_days}


def cmd_session(args: argparse.Namespace) -> dict:
    """Forensic dump of a single session."""
    con = connect(args.db)
    cur = con.execute(
        f"""SELECT id, slug, title, agent, directory, parent_id,
                  summary_additions, summary_deletions, summary_files,
                  time_created, time_updated
           FROM {SESSION_TABLE} WHERE id = ?""",
        (args.session_id,),
    )
    row = cur.fetchone()
    if not row:
        sys.exit(f"ERROR: session {args.session_id} not found")
    s = dict(row)

    parent = None
    if s["parent_id"]:
        cur = con.execute(
            f"SELECT id, slug, title, agent, directory FROM {SESSION_TABLE} WHERE id = ?",
            (s["parent_id"],),
        )
        prow = cur.fetchone()
        if prow:
            parent = dict(prow)

    cur = con.execute(
        f"""SELECT id, slug, title, agent, directory FROM {SESSION_TABLE}
           WHERE parent_id = ? ORDER BY time_created""",
        (args.session_id,),
    )
    children = [dict(r) for r in cur.fetchall()]

    detail = build_primary_detail(
        con,
        s,
        argparse.Namespace(
            limit_user_msgs=args.limit_user_msgs,
            limit_asst_texts=args.limit_asst_texts,
            limit_shell_cmds=args.limit_shell_cmds,
            limit_kid_titles=args.limit_kid_titles,
            text_truncate=args.text_truncate,
            limit_timeline=args.limit_timeline,
            limit_tool_use_samples=args.limit_tool_use_samples,
            shell_output_preview=args.shell_output_preview,
            limit_reasoning=args.limit_reasoning,
            reasoning_truncate=args.reasoning_truncate,
        ),
    )

    return {
        **detail,
        "parent": parent,
        "children": children,
        "events": q_session_events(con, args.session_id),
    }


# ---------- CLI ----------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="opencode_audit.py",
        description="Data layer для аудита opencode-сессий. JSON в stdout.",
    )
    p.add_argument(
        "--db",
        type=Path,
        default=DEFAULT_DB,
        help=f"Path to opencode.db (default: {DEFAULT_DB})",
    )
    p.add_argument(
        "--wiki",
        type=Path,
        default=DEFAULT_WIKI,
        help=f"Path to wiki/ dir (default: {DEFAULT_WIKI})",
    )
    p.add_argument(
        "--compact", action="store_true", help="Compact JSON output (no pretty-print)"
    )
    p.add_argument("--limit-user-msgs", type=int, default=LIMIT_USER_MSGS)
    p.add_argument("--limit-asst-texts", type=int, default=LIMIT_ASST_TEXTS)
    p.add_argument("--limit-shell-cmds", type=int, default=LIMIT_SHELL_CMDS)
    p.add_argument("--limit-kid-titles", type=int, default=LIMIT_KID_TITLES)
    p.add_argument("--text-truncate", type=int, default=TEXT_TRUNCATE)
    p.add_argument("--limit-timeline", type=int, default=LIMIT_TIMELINE)
    p.add_argument("--limit-tool-use-samples", type=int, default=LIMIT_TOOL_USE_SAMPLES)
    p.add_argument("--shell-output-preview", type=int, default=SHELL_OUTPUT_PREVIEW)
    p.add_argument("--limit-reasoning", type=int, default=LIMIT_REASONING)
    p.add_argument("--reasoning-truncate", type=int, default=REASONING_TRUNCATE)

    sub = p.add_subparsers(dest="cmd", required=True)

    p_over = sub.add_parser("overview", help="Aggregate stats for a period")
    p_over.add_argument(
        "--days", type=int, default=7, help="Look back N days (default: 7)"
    )
    p_over.add_argument(
        "--since", type=str, help="Start date YYYY-MM-DD (overrides --days)"
    )
    p_over.add_argument(
        "--until", type=str, help="End date YYYY-MM-DD (default: today)"
    )
    p_over.set_defaults(func=cmd_overview)

    p_day = sub.add_parser("day", help="Detailed dump of a single day")
    p_day.add_argument("date", type=parse_date, help="Day YYYY-MM-DD")
    p_day.set_defaults(func=cmd_day)

    p_range = sub.add_parser("range", help="Detailed dump of a date range (inclusive)")
    p_range.add_argument("start", type=parse_date, help="Start YYYY-MM-DD")
    p_range.add_argument("end", type=parse_date, help="End YYYY-MM-DD")
    p_range.set_defaults(func=cmd_range)

    p_sess = sub.add_parser("session", help="Forensic dump of a single session")
    p_sess.add_argument("session_id", help="session id (ses_...)")
    p_sess.set_defaults(func=cmd_session)

    return p


def main() -> None:
    args = build_parser().parse_args()
    result = args.func(args)
    indent = None if args.compact else 2
    print(json.dumps(result, ensure_ascii=False, indent=indent))


if __name__ == "__main__":
    main()
