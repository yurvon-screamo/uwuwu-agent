# relay (rathole-server) — туннель Ollama kisa → Railway.
# Railway-сервис `relay` деплоится из корня репо (source = master):
# uwuwu-server билдится railway up из server/ — этот Dockerfile его
# не касается. Переменные сервиса: RATHOLE_TOKEN, PORT (2333).
#
# ВАЖНО: rapiz1/rathole:latest — distroless (без /bin/sh): конфиг в
# рантайме генерить нечем, `CMD ["/bin/sh", …]` умирает мгновенно без
# логов (деплои 15.11 06:10/06:28 UTC). Multi-stage: бинаррь из
# официального образа + debian:12-slim. Бинаррь ДИНАМИЧЕСКИЙ — требует
# libssl.so.3/libcrypto.so.3 (пакет libssl3; деплой 09:10 падал на
# «error while loading shared libraries: libssl.so.3»).
FROM docker.io/rapiz1/rathole:latest AS rathole
FROM docker.io/library/debian:12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=rathole /app/rathole /usr/local/bin/rathole
COPY server/ops/relay/entrypoint.sh /entrypoint.sh
ENTRYPOINT []
CMD ["/bin/sh", "/entrypoint.sh"]
# Healthcheck: control-порт жив (rathole слушает). Без него Railway держал
# деплой SUCCESS при мгновенном падении процесса (crash-loop 15.11).
# bash /dev/tcp — в dash этой фичи нет, зовём bash явно.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/2333' || exit 1
