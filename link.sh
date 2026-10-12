#!/usr/bin/env bash
# link.sh — идемпотентная привязка конфигов опенкода к этому репозиторию.
# Файлы живут ЗДЕСЬ; ~/.config/opencode и ~/.agents/.claude получают симлинки.
# Повторный запуск безопасен. Существующие не-симлинки бэкапятся (*.pre-link-*).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

link() {
  local target="$1" link="$2"
  mkdir -p "$(dirname "$link")"
  if [ -e "$link" ] && [ ! -L "$link" ]; then
    local bak="${link}.pre-link-$(date +%Y%m%d-%H%M%S)"
    mv "$link" "$bak"
    echo "бэкап: $link -> $bak"
  fi
  ln -sfn "$target" "$link"
  echo "link: $link -> $(readlink "$link")"
}

link "$SCRIPT_DIR/opencode.json" ~/.config/opencode/opencode.json
link "$SCRIPT_DIR/cli.json"      ~/.config/opencode/cli.json
link "$SCRIPT_DIR/AGENTS.md"     ~/.config/opencode/AGENTS.md
link "$SCRIPT_DIR/agents"        ~/.config/opencode/agents
link "$SCRIPT_DIR/plugins"       ~/.config/opencode/plugins
link "$SCRIPT_DIR/skills"        ~/.agents/skills
link "$SCRIPT_DIR/AGENTS.md"     ~/.agents/AGENTS.md
link "$SCRIPT_DIR/skills"        ~/.claude/skills

echo "done. перезапусти opencode."
