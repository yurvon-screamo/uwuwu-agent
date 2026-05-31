#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

: "${GITLAB_PERSONAL_ACCESS_TOKEN:?Error: GITLAB_PERSONAL_ACCESS_TOKEN not set. Export it or add to .env}"

export GITLAB_API_URL="${GITLAB_API_URL:-https://gitlab.rusklimat.ru}"
export GITLAB_READ_ONLY_MODE="${GITLAB_READ_ONLY_MODE:-false}"
export USE_GITLAB_WIKI="${USE_GITLAB_WIKI:-false}"
export USE_MILESTONE="${USE_MILESTONE:-false}"
export USE_PIPELINE="${USE_PIPELINE:-true}"
bun "$SCRIPT_DIR/gitlab.ts" "$@"
