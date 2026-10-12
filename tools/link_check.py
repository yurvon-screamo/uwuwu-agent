#!/usr/bin/env python3
"""link_check — валидатор markdown-линков вики (experience/ + projects/).

Проверяет относительные .md-ссылки [..](..md..): цель существует и внутри
вики-корня. HTTP(S)-ссылки не трогает. Анкоры (#foo) игнорируются.
Ограничения: POSIX-пути (виндовые \\ не резолвятся); фенсы считаются
закрытыми парами (незакрытый — предупреждение).

usage:
  link_check.py [--root PATH]   # дефолт: UWUWU_WIKI или ~/.uwuwu
Выход: rc=0 (чисто) / rc=1 (есть битые, список в stdout).
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]+?\.md(?:#[^)]*)?)\)")
FENCE_RE = re.compile(r"```.*?```", re.DOTALL)

ZONES = ("experience", "projects")


def wiki_root() -> Path:
    return Path(os.environ.get("UWUWU_WIKI", str(Path.home() / ".uwuwu")))


def check(root: Path) -> list[tuple[str, str, str]]:
    broken: list[tuple[str, str, str]] = []
    base = root.resolve()
    files = [f for z in ZONES for f in (root / z).rglob("*.md")]
    for f in files:
        text = f.read_text(encoding="utf-8")
        # Фенс-разделитель CommonMark: до 3 ведущих пробелов + ``` в НАЧАЛЕ строки.
        # Упоминания `` ``` `` в середине строки — inline-код, не фенсы.
        fence_lines = sum(
            1 for ln in text.splitlines() if ln.lstrip(" \t")[:3] == "```"
        )
        if fence_lines % 2:
            print(f"WARN: {f}: нечётное число фенсов — хвост файла не проверен")
        text = FENCE_RE.sub("", text)  # примеры в код-фенсах — не наши линки
        for _, target in LINK_RE.findall(text):
            if target.startswith(("http://", "https://")):
                continue
            clean = target.split("#")[0]
            if not clean:
                continue
            resolved = (f.parent / clean).resolve()
            try:
                resolved.relative_to(base)
            except ValueError:
                broken.append((str(f), target, "вне вики"))
                continue
            if not resolved.exists():
                broken.append((str(f), target, "нет файла"))
    return broken


def main() -> int:
    root = wiki_root()
    if "--root" in sys.argv:
        i = sys.argv.index("--root")
        if i + 1 >= len(sys.argv):
            print("--root требует значение: путь к вики", file=sys.stderr)
            return 2
        root = Path(sys.argv[i + 1])
    broken = check(root)
    if broken:
        for f, t, why in broken:
            print(f"BIT: {f} -> {t} ({why})")
        print(f"\nитого битых: {len(broken)}")
        return 1
    print("линки чисты")
    return 0


if __name__ == "__main__":
    sys.exit(main())
