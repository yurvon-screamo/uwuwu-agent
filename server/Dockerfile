# uwuwu-server: образ Railway (root directory сервиса = server/)
# Workspace: crates/{domain,usecase,server}; protoc — свежий бинаррь для lance-стека
FROM rust:slim AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl unzip ca-certificates \
    && curl -sSL "https://github.com/protocolbuffers/protobuf/releases/download/v29.3/protoc-29.3-linux-x86_64.zip" -o /tmp/protoc.zip \
    && unzip -o /tmp/protoc.zip -d /usr/local bin/protoc 'include/*' \
    && chmod +x /usr/local/bin/protoc \
    && apt-get purge -y curl unzip && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# BuildKit cache mounts: registry + target переживают билды —
# инкрементальная сборка вместо полной пересборки lance/arrow/tokio
# на каждую правку (было ~20 мин, с кешем точечные правки ~3-5 мин).
# Гоча: target под cache mount не попадает в слои — бинарь копируем
# в образ из RUN до его завершения.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin uwuwu-server \
    && cp /build/target/release/uwuwu-server /build/uwuwu-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/uwuwu-server /usr/local/bin/uwuwu-server
COPY config.example.toml /app/config.toml
WORKDIR /app
ENV UWUWU_SERVER_CONFIG=/app/config.toml
EXPOSE 8090
# Healthcheck: HTTP-порт жив. /health под bearer-гейтом (401 без токена) —
# платформенный HTTP-healthcheck не подошёл бы, проверяем TCP; bash /dev/tcp.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8090' || exit 1
CMD ["uwuwu-server"]
