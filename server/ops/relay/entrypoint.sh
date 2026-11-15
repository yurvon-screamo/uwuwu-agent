#!/bin/sh
# rathole-server из env (токен не в репо): конфиг генерится в рантайме.
# Семантика rathole: [server].bind_addr — control-канал клиентов (публичный
# TCP-прокси Railway), [server.services.X].bind_addr — ОБЯЗАТЕЛЬНЫЙ свой порт
# для посетителей туннеля. Сервис ollama на 2334 (эмбеддинги) и opencode
# на 2335 (исполнитель-гейт) видны только внутри Railway
# (relay.railway.internal:23XX), в интернет не торчат — наружу смотрит
# лишь control-порт (TCP proxy → rathole-клиенты kisa).
set -e
: "${RATHOLE_TOKEN:?RATHOLE_TOKEN не задан}"
: "${PORT:?PORT не задан}"
printf '[server]\nbind_addr="0.0.0.0:%s"\n\n[server.services.ollama]\ntoken="%s"\nbind_addr="0.0.0.0:2334"\n\n[server.services.opencode]\ntoken="%s"\nbind_addr="0.0.0.0:2335"\n' \
  "$PORT" "$RATHOLE_TOKEN" "$RATHOLE_TOKEN" > /tmp/rathole.toml
exec rathole /tmp/rathole.toml
