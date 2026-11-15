# uwuwu-server: образ Railway (root directory сервиса = server/)
# Workspace: crates/{domain,usecase,server}; protoc — свежий бинаррь для lance-стека
FROM rust:slim AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl unzip ca-certificates \
    && curl -sSL "https://github.com/protocolbuffers/protobuf/releases/download/v29.3/protoc-29.3-linux-x86_64.zip" -o /tmp/protoc.zip \
    && unzip -o /tmp/protoc.zip -d /usr/local bin/protoc \
    && chmod +x /usr/local/bin/protoc \
    && apt-get purge -y curl unzip && apt-get autoremove -y \
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
ENV UWUWU_SERVER_CONFIG=/app/config.toml
EXPOSE 8090
CMD ["uwuwu-server"]
