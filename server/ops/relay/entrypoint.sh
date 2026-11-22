#!/bin/sh
# rathole-server из env (токен не в репо): конфиг генерируется в рантайме.
# Семантика rathole: [server].bind_addr — control-канал клиентов (публичный
# TCP-прокси Railway), [server.services.X].bind_addr — ОБЯЗАТЕЛЬНЫЙ свой порт
# для посетителей туннеля. Сервисы opencode на 2335 (исполнитель-гейт kisa)
# и opencode2 на 2336 (исполнитель redmi) видны только внутри Railway
# (relay.railway.internal:23XX), в интернет не торчат — наружу смотрит
# лишь control-порт (TCP proxy → rathole-клиенты исполнителей).
# Легаси ollama-сервис (:2334) снесён — эмбеддинги переехали на
# Gemini API (#1363).
set -e
: "${RATHOLE_TOKEN:?RATHOLE_TOKEN не задан}"
: "${PORT:?PORT не задан}"
printf '[server]\nbind_addr="0.0.0.0:%s"\n\n[server.services.opencode]\ntoken="%s"\nbind_addr="0.0.0.0:2335"\n\n[server.services.opencode2]\ntoken="%s"\nbind_addr="0.0.0.0:2336"\n' \
  "$PORT" "$RATHOLE_TOKEN" "$RATHOLE_TOKEN" > /tmp/rathole.toml
exec rathole /tmp/rathole.toml
