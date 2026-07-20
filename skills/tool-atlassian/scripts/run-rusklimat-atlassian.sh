#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

: "${CONFLUENCE_PERSONAL_TOKEN:?Error: CONFLUENCE_PERSONAL_TOKEN not set. Export it or add to .env}"
export CONFLUENCE_USERNAME="turbin_y@rusklimat.ru"
: "${JIRA_PERSONAL_TOKEN:?Error: JIRA_PERSONAL_TOKEN not set. Export it or add to .env}"
export JIRA_USERNAME="turbin_y@rusklimat.ru"

export CONFLUENCE_URL="${CONFLUENCE_URL:-https://wiki.rusklimat.ru}"
export JIRA_URL="${JIRA_URL:-https://jira.rusklimat.ru/}"
bun "$SCRIPT_DIR/atlassian.ts" "$@"
