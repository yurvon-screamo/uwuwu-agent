# uwuwu-server: образ Railway (root directory сервиса = server/)
FROM rust:slim AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/uwuwu-server /usr/local/bin/uwuwu-server
COPY config.example.toml /app/config.toml
WORKDIR /app
# Конфиг в образе — канон-пример; секреты (GH_TOKEN, UWUWU_SERVER_BEARER)
# и точечные оверрайды — Railway env, не в образе
ENV UWUWU_SERVER_CONFIG=/app/config.toml
EXPOSE 8090
CMD ["uwuwu-server"]
