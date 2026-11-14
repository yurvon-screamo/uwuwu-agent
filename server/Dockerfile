# uwuwu-server: образ Railway (root directory сервиса = server/)
# Workspace: crates/{domain,usecase,server}; protoc нужен lance-encoding
FROM rust:slim AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin uwuwu-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/uwuwu-server /usr/local/bin/uwuwu-server
COPY config.example.toml /app/config.toml
WORKDIR /app
# Конфиг в образе — канон-пример; секреты (GH_TOKEN, UWUWU_SERVER_BEARER,
# UWUWU_OPENCODE_PASSWORD, UWUWU_SERVER_MODE, UWUWU_SERVER_STATE) — Railway env
ENV UWUWU_SERVER_CONFIG=/app/config.toml
EXPOSE 8090
CMD ["uwuwu-server"]
