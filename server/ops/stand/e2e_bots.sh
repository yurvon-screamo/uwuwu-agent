#!/usr/bin/env bash
# e2e_bots.sh — мок-стенд бот-контура uwuwu-server (#1150): док-спека в
# вики-памяти → супервизор поднимает бота → ТГ-сообщение через mock_tg →
# reply вечной сессии (mock_opencode) → sendMessage владельцу.
#
# Полный цикл «один док = один бот» без GitHub/LLM/ТГ:
#   mock_tg + mock_opencode + uwuwu-server (memory hub); порты —
#   подбор/оверрайд, см. блок портов ниже.
#
# Запуск: server/ops/stand/e2e_bots.sh   (из каталога server; cargo build сам)
# Выход: 0 — ок; 1 — провал с диагнозом. Логи: /tmp/uwuwu-stand-bots-*.log
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
# Порты: оверрайды E2E_PORT_* заданы — как есть (занятость на зовущем);
# не заданы — свободные из пробы-бинда: гонка параллельных стендов снята
# классом (инцидент 17.11: чужой e2e занимал дефолтные 8090–8099, #1304)
# stand_ports.sh подключается по пути рантайма ($(dirname "$0")) — статически
# не следует; сам файл валиден отдельно (SC1091 off на этой строке, #1304)
# shellcheck disable=SC1091
source "$(dirname "$0")/stand_ports.sh"
ports="$(stand_pick_ports 3)"
read -r PORT_TG_D PORT_MOCK_D PORT_SRV_D <<<"$ports"
PORT_TG="${E2E_PORT_TG:-$PORT_TG_D}"; PORT_MOCK="${E2E_PORT_MOCK:-$PORT_MOCK_D}"; PORT_SRV="${E2E_PORT_SRV:-$PORT_SRV_D}"
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
TOKEN="tok-e2e-probe"
# логи per-run (суффикс порта): параллельные стенды не пишут в один файл
LOG_TG=/tmp/uwuwu-stand-bots-$PORT_TG-tg.log
LOG_MOCK=/tmp/uwuwu-stand-bots-$PORT_MOCK-mock.log
LOG_SRV=/tmp/uwuwu-stand-bots-$PORT_SRV-server.log
STATE=ops/state.json

say()  { printf '\033[1;34m[e2e-bots]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e-bots FAIL]\033[0m %s\n' "$*"; exit 1; }

# PID-гигиена: гасим только свои процессы (чужие стенды на том же
# бинаре не трогаем)
SRV_PID=""; MOCK_PID=""; TG_PID=""
cleanup() { for pid in "$SRV_PID" "$MOCK_PID" "$TG_PID"; do
              [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
            done; rm -f ops/config.memory.e2e.toml; }
trap cleanup EXIT

mcp() { # MCP-вызов на локальный сервер (health-проба)
  curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"person_show","arguments":{}}}' >/dev/null
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
rm -f "$STATE"

# конфиг стенда с executor-url прогона (дефолтный config.memory.toml не
# трогаем; TOML-ключ port читается — config.rs: port() = env PORT > TOML
# port > дефолт, серверный порт по-прежнему задаётся env-ом при запуске ниже)
sed -e "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|" \
    ops/config.memory.toml > ops/config.memory.e2e.toml

say "стенд: mock_tg ($PORT_TG) + mock_opencode ($PORT_MOCK) + сервер ($PORT_SRV)"
MOCK_TG_PORT=$PORT_TG python3 ops/stand/mock_tg.py >"$LOG_TG" 2>&1 &
TG_PID=$!
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=200 \
  MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV/mcp" \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1

# вики-док бота: «один док = один бот», токен только в env стенда
BOT_DOC='{"bot/Проба-раннера/README": "---\ntitle: Проба раннера\nmodel: opencode-go/hy4-flash-free\ntoken_env: UWUWU_BOT_TOKEN_E2E\nowner: yurvon\nworkspace: /tmp/opencode\ngreeting: привет от стенда\n---\n\nОтвечай кратко, одной строкой."}'

say "сервер (live, memory-hub, вики в памяти)"
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.e2e.toml \
PORT=$PORT_SRV \
UWUWU_SERVER_MODE=live \
UWUWU_SERVER_BEARER="$BEARER" \
UWUWU_OPENCODE_PASSWORD=mockpass \
UWUWU_TG_API="http://127.0.0.1:$PORT_TG" \
UWUWU_BOT_TOKEN_E2E="$TOKEN" \
UWUWU_MOCK_DOCS="$BOT_DOC" \
RUST_LOG=info \
  ./target/debug/uwuwu-server >"$LOG_SRV" 2>&1 &
SRV_PID=$!

# супервизор спавнит бота на первом тике
say "ожидание bot.spawned…"
for _ in $(seq 1 20); do
  sleep 1
  grep -q '"event":"bot.spawned"' "$LOG_SRV" && break
done
grep -q '"event":"bot.spawned"' "$LOG_SRV" || fail "супервизор не поднял бота (лог: $LOG_SRV)"
grep -q '"event":"bot.spawned"' "$LOG_SRV" && say "бот поднят: док-спека → рантайм"

# ТГ-сообщение владельца → бот → вечная сессия → reply → sendMessage
say "инжект ТГ-сообщения…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":1,"chat_id":42,"username":"yurvon","text":"e2e ping"}' >/dev/null

REPLY=""
for _ in $(seq 1 40); do
  sleep 1
  REPLY=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'e2e ping' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$REPLY" ] && break
done
[ -n "$REPLY" ] || fail "реплай не дошёл до ТГ за 40с (лог: $LOG_SRV)"
say "реплай доставлен: $REPLY"

# /start — детерминированный greeting без LLM
say "инжект /start…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":2,"chat_id":42,"username":"yurvon","text":"/start"}' >/dev/null
GREET=""
for _ in $(seq 1 20); do
  sleep 1
  GREET=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'привет от стенда' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$GREET" ] && break
done
[ -n "$GREET" ] || fail "greeting на /start не отправлен"
say "greeting доставлен: $GREET"

# state.json: секция bots с session+offset — рестарт восстанавливает
say "проверка state.json…"
python3 - "$STATE" <<'PY'
import json, sys
st = json.load(open(sys.argv[1]))
bots = st.get("bots") or {}
row = bots.get("bot/Проба-раннера")
assert row, f"секция bots пуста: {list(bots)}"
assert row.get("session"), "нет сессии в state"
assert row.get("offset"), "нет offset в state"
print(f"[e2e-bots] state ok: session={row['session'][:16]}… offset={row['offset']}")
PY

# рестарт сервера: боты восстанавливаются с памятью (liveness Alive → реюз)
say "рестарт сервера…"
kill "$SRV_PID" 2>/dev/null || true
sleep 1
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.e2e.toml \
PORT=$PORT_SRV \
UWUWU_SERVER_MODE=live \
UWUWU_SERVER_BEARER="$BEARER" \
UWUWU_OPENCODE_PASSWORD=mockpass \
UWUWU_TG_API="http://127.0.0.1:$PORT_TG" \
UWUWU_BOT_TOKEN_E2E="$TOKEN" \
UWUWU_MOCK_DOCS="$BOT_DOC" \
RUST_LOG=info \
  ./target/debug/uwuwu-server >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
CREATED_BEFORE=$(grep -c '"event":"bot.session_created"' "$LOG_SRV" || true)
for _ in $(seq 1 20); do
  sleep 1
  grep -q '"event":"bot.started"' "$LOG_SRV" && break
done
sleep 10   # тик супервизора после рестарта
CREATED_AFTER=$(grep -c '"event":"bot.session_created"' "$LOG_SRV" || true)
[ "$CREATED_AFTER" -eq "$CREATED_BEFORE" ] || fail "после рестарта сессия пересоздана ($CREATED_BEFORE → $CREATED_AFTER)"
say "рестарт: бот восстановлен, сессия переиспользована (созданий: $CREATED_AFTER)"

# ---------- автокомпакт (#1221): пороги стенда (idle 0, pct 0) → нативный compact ----------
say "ожидание bot.compacted…"
for _ in $(seq 1 20); do
  sleep 1
  grep -q '"event":"bot.compacted"' "$LOG_SRV" && break
done
grep -q '"event":"bot.compacted"' "$LOG_SRV" || fail "компакт не admitted за 20с (лог: $LOG_SRV)"
grep -q '"event":"mock.session_compacted"' "$LOG_MOCK" || fail "мок не принял POST /compact"
say "компакт admitted: мок принял заявку"

# коулдаун (1 мин): повторной адмиссии в окне нет
sleep 15
COMPACTS=$(grep -c '"event":"bot.compacted"' "$LOG_SRV" || true)
[ "$COMPACTS" = "1" ] || fail "повторный компакт в коулдауне ($COMPACTS раз)"
say "коулдаун: повторной адмиссии нет"

# живость после компакта: бот отвечает на следующее сообщение владельца
say "инжект сообщения после компакта…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":3,"chat_id":42,"username":"yurvon","text":"после компакта"}' >/dev/null
POST_REPLY=""
for _ in $(seq 1 40); do
  sleep 1
  POST_REPLY=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'после компакта' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$POST_REPLY" ] && break
done
[ -n "$POST_REPLY" ] || fail "реплай после компакта не дошёл до ТГ (лог: $LOG_SRV)"
say "живость после компакта: $POST_REPLY"

# многосообщный ход (#1209): «turn probe» → фазы пробы → только финал в ТГ
say "инжект turn probe (двухсообщный ход)…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":4,"chat_id":42,"username":"yurvon","text":"turn probe"}' >/dev/null
TURN=""
for _ in $(seq 1 20); do
  sleep 1
  TURN=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'turn final ok' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$TURN" ] && break
done
[ -n "$TURN" ] || fail "финал turn-хода не дошёл до ТГ за 20с (лог: $LOG_SRV)"
INTERM=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
print(sum(1 for m in sent if 'промежуточный шаг' in m['text']))" 2>/dev/null)
[ "$INTERM" = "0" ] || fail "промежуточный шаг хода ушёл в ТГ ($INTERM шт) — тихая обрезка вернулась"
say "turn-ход доставлен: только финал, промежуточный шаг не уходит"

# групповое сообщение поглощается (private-only)
say "инжект группового сообщения…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":5,"chat_id":-100,"chat_type":"group","username":"yurvon","text":"из группы"}' >/dev/null
sleep 5
grep -q '"event":"bot.skipped"' "$LOG_SRV" || fail "групповое сообщение не поглощено (bot.skipped нет)"
say "группа поглощена: private-only работает"

# ---------- media-аттачи (#1171): фото → getFile → URL в промпте ----------
say "инжект фото владельца…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":6,"chat_id":42,"username":"yurvon","media":{"kind":"photo","file_id":"photo-e2e","file_size":2048,"caption":"скрин для стенда"}}' >/dev/null
MEDIA_OK=""
for _ in $(seq 1 20); do
  sleep 1
  grep -q '"event":"bot.media_delivered"' "$LOG_SRV" && MEDIA_OK=1 && break
done
[ -n "$MEDIA_OK" ] || fail "фото не доставлено в сессию (bot.media_delivered нет; лог: $LOG_SRV)"
# эхо-реплай сессии = промпт с URL доехал (последняя строка промпта — инструкция)
MEDIA_REPLY=""
for _ in $(seq 1 20); do
  sleep 1
  MEDIA_REPLY=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'вложи эту ссылку' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$MEDIA_REPLY" ] && break
done
[ -n "$MEDIA_REPLY" ] || fail "реплай с media-инструкцией не дошёл до ТГ"
say "фото доставлено: getFile → asset → URL в промпте сессии"

# oversize без file_size в апдейте: pre-check молчит, getFile отвечает
# 400 too big (эмуляция mock) → типизированный TelegramTooBig → явный отказ
say "инжект oversized-документа (без file_size — 400-маршрут getFile)…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":7,"chat_id":42,"username":"yurvon","media":{"kind":"document","file_id":"big-e2e","too_big":true,"file_name":"big.bin"}}' >/dev/null
OVER=""
for _ in $(seq 1 20); do
  sleep 1
  OVER=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'слишком большой' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$OVER" ] && break
done
[ -n "$OVER" ] || fail "oversize-отказ владельцу не отправлен (лог: $LOG_SRV)"
grep -q '"kind":"get_file_400"' "$LOG_SRV" || fail "400-маршрут getFile не прогнан (bot.media_oversize kind=get_file_400 нет)"
say "oversize: getFile 400 (эмуляция mock) → типизированный отказ владельцу"

# не-текстовый тип (видео): явный отказ «не принимаю»
say "инжект видео…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":8,"chat_id":42,"username":"yurvon","media":{"kind":"video","file_id":"vid-e2e","file_size":5000}}' >/dev/null
KIND=""
for _ in $(seq 1 20); do
  sleep 1
  KIND=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'не принимаю' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$KIND" ] && break
done
[ -n "$KIND" ] || fail "отказ о не-поддержанном типе не отправлен (лог: $LOG_SRV)"
say "видео: явный отказ, тихой потери нет"

# ---------- форварды (#1416): пересланный пост → факт в промпте сессии ----------
# Наблюдаемость: реплай мока несёт только последнюю строку промпта, маркер —
# первая; mock_opencode логирует head-строку промпта — маркер грепается в LOG_MOCK
say "инжект форварда владельца (канал)…"
curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
  -d '{"update_id":9,"chat_id":42,"username":"yurvon","text":"пост про nginx","forward_origin":{"type":"channel","chat":{"id":-100123,"type":"channel","title":"Nginx Gems","username":"nginx"}}}' >/dev/null
FWD=""
for _ in $(seq 1 20); do
  sleep 1
  grep -q "Пересланный пост — канал @nginx" "$LOG_MOCK" && FWD=1 && break
done
[ -n "$FWD" ] || fail "форвард-факт не доехал до промпта сессии (лог: $LOG_MOCK, $LOG_SRV)"
say "форвард доставлен: маркер источника в промпте бот-сессии"

# реплай на форвард доходит владельцу (явный исход, не тихое оседание)
FWD_REPLY=""
for _ in $(seq 1 20); do
  sleep 1
  FWD_REPLY=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c "
import json,sys
sent=json.load(sys.stdin)['sent']
hits=[m['text'] for m in sent if 'пост про nginx' in m['text']]
print(hits[0] if hits else '')" 2>/dev/null)
  [ -n "$FWD_REPLY" ] && break
done
[ -n "$FWD_REPLY" ] || fail "реплай на форвард не дошёл до ТГ"
say "форвард: реплай владельцу отправлен"

say "ИТОГ: E2E ботов зелёный (spawn → reply → greeting → state → рестарт → private-only → media → forward)"
