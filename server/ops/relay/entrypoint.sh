#!/bin/sh
# rathole-server из env (токен не в репо): конфиг генерится в рантайме.
set -e
: "${RATHOLE_TOKEN:?RATHOLE_TOKEN не задан}"
: "${PORT:?PORT не задан}"
printf '[server]\nbind_addr="0.0.0.0:%s"\n\n[server.services.ollama]\ntoken="%s"\n' \
  "$PORT" "$RATHOLE_TOKEN" > /tmp/rathole.toml
exec rathole /tmp/rathole.toml
