# relay (rathole-server) — туннель Ollama kisa → Railway.
# Railway-сервис `relay` деплоится из корня репо (source = master):
# этот Dockerfile делегирует в server/ops/relay (uwuwu-server билдится
# railway up из server/ — корневой Dockerfile его не касается).
# Переменные сервиса: RATHOLE_TOKEN, PORT (2333).
# ВАЖНО: базовый образ имеет ENTRYPOINT ["rathole"] — сбрасываем,
# иначе CMD становится аргументами rathole.
FROM docker.io/rapiz1/rathole:latest
ENTRYPOINT []
COPY server/ops/relay/entrypoint.sh /entrypoint.sh
CMD ["/bin/sh", "/entrypoint.sh"]
