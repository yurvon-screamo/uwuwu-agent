#!/usr/bin/env bash
# e2e_bots_lessons.sh — мок-стенд доставки уроков бота (#1424): соседний док
# bot/<имя>/Уроки доезжает до вечной сессии с ближайшим ходом.
#
#   doc-режим   — композит «методичка + блок уроков» через hash-гейт manual;
#   agent-режим — lessons-hash в state: префикс при смене носителя, промпт
#                 отзыва при опустошении; hash переживает рестарт.
#   Бот без дока уроков — поведение как прежде (регресс-гейт).
#
# Ночной append крона «Дример дримеров» эмулируется живой записью дока
# через MCP стенда (memory-hub поддерживает docs_write/docs_append).
#
# Запуск: server/ops/stand/e2e_bots_lessons.sh   (из каталога server; cargo build сам)
# Порты env-перезаписываемые: параллельные стенды разных задач не дерутся.
# Компакт в стенде отключён (пороги в потолок) — гейты доставки
# детерминированы, компакт-реарм покрыт юнит-тестами раннера.
# Выход: 0 — ок; 1 — провал с диагнозом. Логи: /tmp/uwuwu-stand-lessons-*.log
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_TG="${E2E_PORT_TG:-8198}"; PORT_MOCK="${E2E_PORT_MOCK:-8199}"; PORT_SRV="${E2E_PORT_SRV:-8190}"
BEARER="${UWUWU_SERVER_BEARER:-lessons-e2e-bearer}"
LOG_TG=/tmp/uwuwu-stand-lessons-$PORT_TG-tg.log
LOG_MOCK=/tmp/uwuwu-stand-lessons-$PORT_MOCK-mock.log
LOG_SRV=/tmp/uwuwu-stand-lessons-$PORT_SRV-server.log
STATE=ops/state.json

say()  { printf '\033[1;34m[e2e-lessons]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e-lessons FAIL]\033[0m %s\n' "$*"; exit 1; }

SRV_PID=""; MOCK_PID=""; TG_PID=""
cleanup() { for pid in "$SRV_PID" "$MOCK_PID" "$TG_PID"; do
              [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
            done; rm -f ops/config.memory.e2e-lessons.toml; }
trap cleanup EXIT

mcp() { # MCP tools/call на стенд ($1 — json аргументов вызова)
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "$1"
}

inject() { # $1 — update_id, $2 — текст
  curl -s -X POST "http://127.0.0.1:$PORT_TG/inject" -H 'Content-Type: application/json' \
    -d "{\"update_id\":$1,\"chat_id\":42,\"username\":\"yurvon\",\"text\":\"$2\"}" >/dev/null
}

wait_replies() { # $1 — сколько суммарно отправок в ТГ ждём
  local n
  for _ in $(seq 1 40); do
    n=$(curl -s "http://127.0.0.1:$PORT_TG/sent" | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["sent"]))' 2>/dev/null || echo 0)
    [ "$n" -ge "$1" ] && return 0
    sleep 1
  done
  fail "в ТГ только $n отправок из $1 (лог: $LOG_SRV)"
}

count_log() { grep -c "$1" "$2" 2>/dev/null || true; }

wait_log() { # $1 файл, $2 паттерн, $3 сколько, $4 таймаут-сек
  local n
  for _ in $(seq 1 "${4:-40}"); do
    n=$(count_log "$2" "$1")
    [ "$n" -ge "$3" ] && return 0
    sleep 1
  done
  fail "в $1 нет $3× «$2» (есть $n)"
}

doc_len() { # len N-й доставки композита (промпт с шапкой методички)
  grep '"event":"mock.prompt"' "$LOG_MOCK" \
    | grep 'The bot manual is the only valid' \
    | sed -E 's/.*"len":([0-9]+).*/\1/' \
    | sed -n "${1}p"
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
rm -f "$STATE"

# конфиг стенда: executor-url прогона + компакт в потолок (гейты доставки
# детерминированы; компакт-реарм уроков покрыт юнит-тестами раннера)
sed -e "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|" \
    -e "s/^idle_min = 0/idle_min = 600/" \
    -e "s/^min_context_pct = 0.0/min_context_pct = 99.0/" \
    ops/config.memory.toml > ops/config.memory.e2e-lessons.toml

say "стенд: mock_tg ($PORT_TG) + mock_opencode ($PORT_MOCK) + сервер ($PORT_SRV)"
MOCK_TG_PORT=$PORT_TG python3 ops/stand/mock_tg.py >"$LOG_TG" 2>&1 &
TG_PID=$!
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=200 \
  MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV/mcp" \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1

# три бота: doc-режим с уроками, agent-режим с уроками, agent-режим без
# дока уроков (регресс-гейт «дока нет — поведение прежнее»)
BOT_DOCS='{
"bot/Урокодок/README": "---\ntitle: Урокодок\nmodel: zai-coding-plan/glm-5.3-flash\ntoken_env: UWUWU_BOT_TOKEN_LESS_D\nowner: yurvon\nworkspace: /tmp/opencode\n---\n\nМетодичка док-бота: отвечай кратко.",
"bot/Урокодок/Уроки": "Урок-1: отвечай по-русски.",
"bot/Урокоагент/README": "---\ntitle: Урокоагент\nmodel: zai-coding-plan/glm-5.3-flash\ntoken_env: UWUWU_BOT_TOKEN_LESS_A\nowner: yurvon\nworkspace: /tmp/opencode\nagent: probe-agent\n---\n\nТело дока игнорируется (методичка в agents/probe-agent.md).",
"bot/Урокоагент/Уроки": "Урок-A: не выдумывай факты.",
"bot/Чистбот/README": "---\ntitle: Чистбот\nmodel: zai-coding-plan/glm-5.3-flash\ntoken_env: UWUWU_BOT_TOKEN_LESS_C\nowner: yurvon\nworkspace: /tmp/opencode\nagent: probe-agent\n---\n\nТело дока игнорируется (методичка в agents/probe-agent.md)."
}'

UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.e2e-lessons.toml \
PORT=$PORT_SRV \
UWUWU_SERVER_MODE=live \
UWUWU_SERVER_BEARER="$BEARER" \
UWUWU_OPENCODE_PASSWORD=mockpass \
UWUWU_TG_API="http://127.0.0.1:$PORT_TG" \
UWUWU_BOT_TOKEN_LESS_D="tok-doc" \
UWUWU_BOT_TOKEN_LESS_A="tok-agt" \
UWUWU_BOT_TOKEN_LESS_C="tok-clean" \
UWUWU_MOCK_DOCS="$BOT_DOCS" \
RUST_LOG=info \
  ./target/debug/uwuwu-server >"$LOG_SRV" 2>&1 &
SRV_PID=$!

say "ожидание трёх bot.spawned…"
wait_log "$LOG_SRV" '"event":"bot.spawned"' 3 30
say "боты подняты: doc+уроки, agent+уроки, agent без уроков"

# ---------- ход 1: бутстрап — уроки доезжают с первым же ходом ----------
say "ход 1: бутстрап-доставка уроков…"
inject 1 "ход раз"
wait_replies 3
wait_log "$LOG_MOCK" 'New bot lessons' 1 10
STATE_LESSONS=$(python3 - "$STATE" <<'PY'
import json, sys
st = json.load(open(sys.argv[1]))
bots = st.get("bots") or {}
agt = bots.get("bot/Урокоагент") or {}
doc = bots.get("bot/Урокодок") or {}
clean = bots.get("bot/Чистбот") or {}
print(f"agt={agt.get('lessons_hash')} doc_manual={'set' if doc.get('manual_hash') else None} clean={clean.get('lessons_hash')}")
PY
)
echo "$STATE_LESSONS" | grep -q "agt=[0-9]" || fail "lessons-hash не персистится в state: $STATE_LESSONS"
echo "$STATE_LESSONS" | grep -q "doc_manual=set" || fail "manual-hash композита не персистится: $STATE_LESSONS"
echo "$STATE_LESSONS" | grep -q "clean=None" || fail "боту без дока уроков нечего помнить: $STATE_LESSONS"
say "бутстрап: префикс agent-бота в промпте; hash-и в state: $STATE_LESSONS"

# ---------- ход 2: гейт — повторной доставки нет ----------
say "ход 2: чистый ход…"
inject 2 "ход два"
wait_replies 6
[ "$(count_log 'New bot lessons' "$LOG_MOCK")" = "1" ] || fail "повторный префикс на неизменном носителе"
say "гейт: префикс один, дальше чистый текст"

# ---------- ход 3: «ночной append» через MCP — дозаправка одним ходом ----------
say "ход 3: append уроков (эмуляция ночного крона)…"
mcp '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"docs_append","arguments":{"slug":"bot/Урокодок/Уроки","line":"Урок-2: не выдумывай факты."}}}' >/dev/null
mcp '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"docs_append","arguments":{"slug":"bot/Урокоагент/Уроки","line":"Урок-B: ссылайся на источники."}}}' >/dev/null
wait_log "$LOG_SRV" '"event":"bot.lessons_updated"' 2 40
# doc-режим: смена композита = смена методички (manual_updated);
# agent-режим: методичка до сессии не доезжает — лог молчит (ревью Low 2)
M_UPD_DOC=$(grep '"event":"bot.manual_updated"' "$LOG_SRV" | grep -c 'Урокодок' || true)
M_UPD_AGT=$(grep '"event":"bot.manual_updated"' "$LOG_SRV" | grep -c 'Урокоагент' || true)
[ "$M_UPD_DOC" -ge 1 ] || fail "doc-режим: смена композита не помечена manual_updated"
[ "$M_UPD_AGT" = "0" ] || fail "agent-режим: лишний manual_updated ($M_UPD_AGT) — методичка не доезжает"
inject 3 "ход три"
wait_replies 9
[ "$(count_log 'New bot lessons' "$LOG_MOCK")" = "2" ] || fail "свежие уроки agent-бота не дозаправились"
D1=$(doc_len 1); D2=$(doc_len 2)
[ -n "$D1" ] && [ -n "$D2" ] || fail "нет двух доставок композита doc-бота"
[ "$D2" -gt "$D1" ] || fail "композит не вырос после append ($D1 → $D2)"
say "дозаправка: agent-префикс ×2; композит $D1 → $D2 байт"

# ---------- ход 4: гейт после дозаправки ----------
say "ход 4: снова чистый ход…"
inject 4 "ход два"
wait_replies 12
[ "$(count_log 'New bot lessons' "$LOG_MOCK")" = "2" ] || fail "префикс задублировался после дозаправки"
say "гейт после дозаправки: чисто"

# ---------- ход 5: опустошение носителя ----------
# MCP docs_write отказывает в пустом content (require_args — «пусто =
# опечатка», дофича платформы): компактизация оставляет whitespace —
# normalize_lessons тримит его в Some("") («честно пусто»)
say "ход 5: компактизация — оба носителя очищены (MCP docs_write)…"
mcp '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"docs_write","arguments":{"slug":"bot/Урокоагент/Уроки","content":" "}}}' >/dev/null
mcp '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"docs_write","arguments":{"slug":"bot/Урокодок/Уроки","content":" "}}}' >/dev/null
wait_log "$LOG_SRV" '"event":"bot.lessons_updated"' 4 40
inject 5 "ход два"
wait_replies 15
# agent-бот: явный промпт отзыва одним сообщением, дальше чисто
wait_log "$LOG_MOCK" 'The bot lessons carrier is now empty' 1 10
inject 6 "ход пять"
wait_replies 18
[ "$(count_log 'The bot lessons carrier is now empty' "$LOG_MOCK")" = "1" ] || fail "промпт отзыва задублировался"
# doc-бот: тихий возврат к голой методичке — композит худеет до базы
D3=$(doc_len 3)
[ -n "$D3" ] || fail "нет третьей доставки композита doc-бота"
[ "$D3" -lt "$D1" ] || fail "после опустошения композит не вернулся к голой методичке ($D3 ≥ $D1)"
say "отзыв: agent — промпт отзыва ×1; doc — тихий возврат ($D2 → $D3 байт)"

# ---------- рестарт: hash-и переживают, текущий носитель доезжает ----------
say "рестарт сервера…"
CREATED_BEFORE=$(count_log '"event":"bot.session_created"' "$LOG_SRV")
kill "$SRV_PID" 2>/dev/null || true
sleep 1
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.e2e-lessons.toml \
PORT=$PORT_SRV \
UWUWU_SERVER_MODE=live \
UWUWU_SERVER_BEARER="$BEARER" \
UWUWU_OPENCODE_PASSWORD=mockpass \
UWUWU_TG_API="http://127.0.0.1:$PORT_TG" \
UWUWU_BOT_TOKEN_LESS_D="tok-doc" \
UWUWU_BOT_TOKEN_LESS_A="tok-agt" \
UWUWU_BOT_TOKEN_LESS_C="tok-clean" \
UWUWU_MOCK_DOCS="$BOT_DOCS" \
RUST_LOG=info \
  ./target/debug/uwuwu-server >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
wait_log "$LOG_SRV" '"event":"bot.started"' 1 30
sleep 12   # тик супервизора после рестарта
CREATED_AFTER=$(count_log '"event":"bot.session_created"' "$LOG_SRV")
[ "$CREATED_AFTER" = "$CREATED_BEFORE" ] || fail "после рестарта сессии пересозданы ($CREATED_BEFORE → $CREATED_AFTER)"
# memory-hub пересеивает исходные доки: носитель «вернулся» к урок-1/урок-A —
# сессия обязана получить актуальный носитель с ближайшим ходом
inject 7 "ход раз"
wait_replies 21
[ "$(count_log 'New bot lessons' "$LOG_MOCK")" = "3" ] || fail "после рестарта agent-бот не получил актуальный носитель"
D4=$(doc_len 4)
[ -n "$D4" ] || fail "после рестарта композит doc-бота не дозаправился"
[ "$D4" = "$D1" ] || fail "композит после рестарта не совпал с исходной доставкой ($D4 ≠ $D1)"
say "рестарт: сессии переиспользованы, актуальный носитель дозаправлен (композит $D4 = $D1)"

say "ИТОГ: E2E уроков зелёный (бутстрап → гейт → append-дозаправка → опустошение/отзыв → рестарт → регресс бота без уроков)"
