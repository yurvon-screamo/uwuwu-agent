#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

rm -rf ~/.agents/skills
rm -rf ~/.config/opencode/agents
rm -rf ~/.config/opencode/plugins

cp -r "$SCRIPT_DIR/skills" ~/.agents/skills
cp "$SCRIPT_DIR/AGENTS.md" ~/.agents/AGENTS.md

mkdir -p ~/.opencodereview
cp -r "$SCRIPT_DIR/opencodereview/." ~/.opencodereview/

cp -r "$SCRIPT_DIR/agents" ~/.config/opencode/agents
cp -r "$SCRIPT_DIR/plugins" ~/.config/opencode/plugins

cp "$SCRIPT_DIR/AGENTS.md" ~/.config/opencode/AGENTS.md
cp "$SCRIPT_DIR/opencode.json" ~/.config/opencode/opencode.json
cp "$SCRIPT_DIR/service.jsonc" ~/.config/opencode/service.jsonc
