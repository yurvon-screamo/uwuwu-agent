#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

rm -rf ~/.agents/skills
rm -rf ~/.config/opencode/agent
rm -rf ~/.config/opencode/tools

cp -r "$SCRIPT_DIR/skills" ~/.agents/skills
cp "$SCRIPT_DIR/AGENTS.md" ~/.agents/AGENTS.md

cp -r "$SCRIPT_DIR/agent" ~/.config/opencode/agent
cp -r "$SCRIPT_DIR/tools" ~/.config/opencode/tools

cp "$SCRIPT_DIR/AGENTS.md" ~/.config/opencode/AGENTS.md
cp "$SCRIPT_DIR/opencode.json" ~/.config/opencode/opencode.json
cp "$SCRIPT_DIR/dcp.jsonc" ~/.config/opencode/dcp.jsonc
cp "$SCRIPT_DIR/package.json" ~/.config/opencode/package.json
