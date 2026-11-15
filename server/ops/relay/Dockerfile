# relay: rathole-server (туннель Ollama kisa → Railway → uwuwu-server).
# Сервис Railway `relay`: source = репо uwuwu-agent, root = server/ops/relay.
# Переменные: RATHOLE_TOKEN (общий секрет), PORT (деф. Railway 2333).
FROM docker.io/rapiz1/rathole:latest
COPY entrypoint.sh /entrypoint.sh
CMD ["/bin/sh", "/entrypoint.sh"]
