# siyuan-relay (Railway)

Публикация SiYuan (ноут) в белый интернет: Bunny DNS `siyuan.uwuwu.net` →
Bunny Pull Zone (uwuwu-siyuan, TLS) → Railway `siyuan-relay-production.up.railway.app`
→ Caddy :8080 → rathole server :8333 (TCP-прокси `zephyr.proxy.rlwy.net:12538`) →
rathole client на ноуте (`rathole-siyuan.service`) → `127.0.0.1:6806` (SiYuan kernel).

- `server.toml.example` — конфиг rathole-сервера (токен синхронен с ноутом)
- ноут-сторона: `~/.config/rathole/siyuan-client.toml` + systemd `rathole-siyuan.service`
- деплой: `railway up --service <id>` из этой директории
- auth-код SiYuan — лоцк-скрин (меняется в UI SiYuan, настройках Access Auth Code)
