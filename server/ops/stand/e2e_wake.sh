#!/usr/bin/env bash
# e2e_wake.sh — E2E wake-канала #1206 на мок-стенде (без GitHub/LLM).
#
# Главный пользовательский сценарий задачи #1206 («коммент, созданный во
# время работы агента, терялся: seen без доставки») — ровно то, что
# прогонялось руками тестировщиком:
#   фаза 1 (fresh): коммент новее активности сессии → доставка на тике,
#     seen (last_comment_id) пишется вместе с доставкой;
#   фаза 2 (in-flight): коммент во время хода агента — активность уходит
#     ВЫШЕ коммента (fresh недостижим навсегда) → доставка сразу после
#     ответа агента (idle-критерий AssistantReply.is_latest), seen —
#     только при доставке (до тика доставки last_comment_id держится на
#     предыдущем); на коде до фикса фаза красная: seen писался без
#     доставки, wakeup.delivered не появлялся никогда;
#   фаза 3 (хвост): два непрочитанных за один ход → ОДИН промпт с обоими
#     телами (tail=2), seen — на последнем;
#   фаза 4 (анти-дубль): без новых комментов повторных доставок нет.
#
# Запуск: server/ops/stand/e2e_wake.sh   (из каталога server; cargo build сам)
# Выход: 0 — ок; 1 — провал с диагнозом. Логи: /tmp/uwuwu-stand-wake-*.log
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_MOCK=8099; PORT_SRV=8090
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
SEED_N="${SEED_N:-2047}"
LOG_MOCK=/tmp/uwuwu-stand-wake-mock.log
LOG_SRV=/tmp/uwuwu-stand-wake-srv.log
STATE=ops/state.json

say()  { printf '\033[1;34m[e2e-wake]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e-wake FAIL]\033[0m %s\n' "$*"; exit 1; }

cleanup() { # только свои пиды: чужие stand-серверы не трогаем
  [ -f /tmp/uwuwu-stand-wake-srv.pid ] && kill "$(cat /tmp/uwuwu-stand-wake-srv.pid)" 2>/dev/null || true
  [ -f /tmp/uwuwu-stand-wake-mock.pid ] && kill "$(cat /tmp/uwuwu-stand-wake-mock.pid)" 2>/dev/null || true
}
trap cleanup EXIT

mcp() { # MCP-вызов на локальный сервер
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}
seen_of()  { python3 -c "import json;print(json.load(open('$STATE'))['rows'].get('Задачи uwuwu:$SEED_N',{}).get('last_comment_id',''))"; }
delivered(){ grep -cE '"event":"wakeup.delivered"' "$LOG_SRV" || true; }
wait_for() { # wait_for <что-ищем-grep'ом> <макс-сек>
  local pat="$1" max="$2" i=0
  while [ "$i" -lt "$max" ]; do
    grep -qE "$pat" "$LOG_SRV" && return 0
    sleep 2; i=$((i+2))
  done
  return 1
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
rm -f "$STATE" "$LOG_MOCK" "$LOG_SRV" /tmp/uwuwu-stand-wake-{srv,mock}.pid

MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=200 RUST_LOG=info \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
echo $! > /tmp/uwuwu-stand-wake-mock.pid
sleep 1
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.toml UWUWU_SERVER_STATE=$PWD/$STATE \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" RUST_LOG=info \
  ./target/debug/uwuwu-server >"$LOG_SRV" 2>&1 &
echo $! > /tmp/uwuwu-stand-wake-srv.pid

say "спавн Design-сессии…"
wait_for '"event":"spawn.done"' 30 || fail "спавн не случился (лог: $LOG_SRV)"
SESSION=$(grep -oE '"event":"spawn.done".*"session":"ses_[a-z0-9]+"' "$LOG_SRV" | grep -oE 'ses_[a-z0-9]+' | head -1)
[ -n "$SESSION" ] || fail "id сессии не найден в spawn.done"

# ── фаза 1: fresh — коммент новее активности ─────────────────────────
say "фаза 1: fresh-доставка (коммент новее активности)"
mcp task_comment "{\"number\":$SEED_N,\"text\":\"wake-e2e: коммент-1 (fresh)\"}" >/dev/null
wait_for '"event":"wakeup.delivered".*"idle":false' 30 \
  || fail "fresh-доставка не случилась"
[ "$(seen_of)" = "mockc-1" ] || fail "seen после доставки: $(seen_of) (ожидалось mockc-1)"
say "фаза 1 ок: wakeup.delivered idle=false, seen=mockc-1"

# ── фаза 2: in-flight — коммент во время хода, доставка после ответа ──
say "фаза 2: in-flight (активность уходит выше коммента, доставка по idle)"
mcp task_comment "{\"number\":$SEED_N,\"text\":\"wake-e2e: коммент-2 (in-flight)\"}" >/dev/null
# «агент начал ход после коммента»: активность сессии уходит выше коммента
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$SESSION/prompt" \
  -H 'Content-Type: application/json' -d '{"text":"агент начал ход"}' >/dev/null
[ "$(seen_of)" = "mockc-1" ] || fail "seen продвинулся БЕЗ доставки: $(seen_of) (ожидалось mockc-1)"
wait_for '"event":"wakeup.delivered".*"idle":true' 30 \
  || fail "in-flight-доставка (idle=true) не случилась — seen без доставки?"
[ "$(seen_of)" = "mockc-2" ] || fail "seen после in-flight-доставки: $(seen_of) (ожидалось mockc-2)"
say "фаза 2 ок: коммент во время хода доставлен сразу после ответа (idle=true), seen=mockc-2 при доставке"

# ── фаза 3: хвост — два непрочитанных одним промптом ─────────────────
say "фаза 3: хвост (два коммента → один промпт, tail=2)"
mcp task_comment "{\"number\":$SEED_N,\"text\":\"wake-e2e: коммент-3 (хвост)\"}" >/dev/null
mcp task_comment "{\"number\":$SEED_N,\"text\":\"wake-e2e: коммент-4 (хвост)\"}" >/dev/null
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$SESSION/prompt" \
  -H 'Content-Type: application/json' -d '{"text":"агент завершил ход"}' >/dev/null
wait_for '"event":"wakeup.delivered".*"tail":2' 30 \
  || fail "хвост-доставка (tail=2) не случилась"
[ "$(seen_of)" = "mockc-4" ] || fail "seen после хвоста: $(seen_of) (ожидалось mockc-4)"
# оба тела в одном промпте: echo мока — последняя строка промпта
REPLY=$(curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session/$SESSION/message" \
  | python3 -c "import json,sys;print(json.load(sys.stdin)[0]['content'][0]['text'])")
case "$REPLY" in *"коммент-4 (хвост)"*) ;; *) fail "echo без тела последнего коммента: $REPLY";; esac
say "фаза 3 ок: tail=2 одним промптом, seen=mockc-4, тело последнего в промпте"

# ── фаза 4: анти-дубль ────────────────────────────────────────────────
say "фаза 4: анти-дубль (два тика без комментов — доставок нет)"
N=$(delivered)
sleep 25
[ "$(delivered)" = "$N" ] || fail "повторная доставка без новых комментов (было $N, стало $(delivered))"
say "фаза 4 ок: дублей нет (строка $SEED_N: доставок $N, новых комментов нет)"

say "ИТОГ: E2E wake-канала зелёный (fresh / in-flight после ответа / хвост одним промптом / анти-дубль / seen только при доставке)"
