# relay (rathole-server) — туннель Ollama kisa → Railway.
# Railway-сервис `relay` деплоится из корня репо (source = master):
# uwuwu-server билдится railway up из server/ — этот Dockerfile его
# не касается. Переменные сервиса: RATHOLE_TOKEN, PORT (2333).
#
# ВАЖНО: rapiz1/rathole:latest — distroless (без /bin/sh): конфиг в
# рантайме генерить нечем, `CMD ["/bin/sh", …]` умирает мгновенно без
# логов (деплои 15.11 06:10/06:28 UTC). Multi-stage: бинаррь из
# официального образа + debian:12-slim (glibc —-binary может быть
# слинкован динамически, musl/alpine не угадываем; sh есть).
FROM docker.io/rapiz1/rathole:latest AS rathole
FROM docker.io/library/debian:12-slim
COPY --from=rathole /app/rathole /usr/local/bin/rathole
COPY server/ops/relay/entrypoint.sh /entrypoint.sh
ENTRYPOINT []
CMD ["/bin/sh", "/entrypoint.sh"]
