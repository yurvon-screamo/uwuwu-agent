# SiYuan relay: caddy (HTTP→local rathole) + rathole server (туннель до ноута).
# Бинарь rathole качается официальным релизом (gnu, статически слинкован достаточно).
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
ARG RATHOLE_VER=v0.5.0
RUN curl -sL "https://github.com/rathole-org/rathole/releases/download/${RATHOLE_VER}/rathole-x86_64-unknown-linux-gnu.zip" -o /tmp/r.zip \
    && python3 -c "import zipfile; zipfile.ZipFile('/tmp/r.zip').extractall('/usr/local/bin')" 2>/dev/null \
    || (unzip -o /tmp/r.zip -d /usr/local/bin) \
    && chmod +x /usr/local/bin/rathole && rm -f /tmp/r.zip
RUN curl -sL "https://caddyserver.com/api/download?os=linux&arch=amd64" -o /usr/local/bin/caddy && chmod +x /usr/local/bin/caddy
COPY Caddyfile /etc/caddy/Caddyfile
COPY server.toml.example /etc/rathole/server.toml
EXPOSE 8080 8333
CMD ["sh", "-c", "/usr/local/bin/rathole /etc/rathole/server.toml & exec /usr/local/bin/caddy run --config /etc/caddy/Caddyfile --adapter caddyfile"]
