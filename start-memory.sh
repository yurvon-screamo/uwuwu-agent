#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEMORY_DIR="$SCRIPT_DIR/memory"
SERVER_PATH="$MEMORY_DIR/node_modules/@tencentdb-agent-memory/memory-tencentdb/src/gateway/server.ts"

if [ ! -f "$SERVER_PATH" ]; then
    echo "Server not found: $SERVER_PATH" >&2
    exit 1
fi

cd "$MEMORY_DIR"
npx tsx "$SERVER_PATH"
