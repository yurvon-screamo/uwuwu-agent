#!/usr/bin/env bash
# e2e.sh — автоматический E2E мок-стенда uwuwu-server (без GitHub/LLM).
#
# Покрывает инцидент #1046 (15.11) комплексно:
#   фаза 1 (observe-яд): строка In Progress с пустым слотом живёт в observe
#     2+ тика — счётчики спавн-петель НЕ заряжаются (state.json чист);
#   фаза 2 (live-цикл с ре-энтри): полный цикл Design → … → Review,
#     где ревью результата один раз даёт request_changes (In Progress →
#     повторный dev-круг → …) — луп-гвард не эскалирует, цикл замыкается;
#   фаза 3 (self-эхо #1204): медленный мок (акт 11с при тике 10с) —
#     собственные MCP-комменты срезаются живьём, эхо-доставок нет.
#
# Запуск: server/ops/stand/e2e.sh   (из каталога server; cargo build сам)
# Выход: 0 — ок; 1 — провал с диагнозом. Логи/state/config — mktemp-рандир
# (печатается в упавших сообщениях). Параллельные прогоны изолированы по
# рандиру; при занятых дефолтных портах: PORT_MOCK=X PORT_SRV=Y bash e2e.sh
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_MOCK="${PORT_MOCK:-8099}"; PORT_SRV="${PORT_SRV:-8090}"
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
SEED_N=2046
RUN_DIR="$(mktemp -d /tmp/uwuwu-stand-XXXXXX)"
LOG_MOCK="$RUN_DIR/mock.log"
LOG_SRV_OBS="$RUN_DIR/server-observe.log"
LOG_SRV_LIVE="$RUN_DIR/server-live.log"
LOG_SRV_ECHO="$RUN_DIR/server-echo.log"
STATE="$RUN_DIR/state.json"
# конфиг под рандир: executor url → порт фазового мока, state.json — рядом
# с конфигом (cfg.state_path: config-adjacent), shared-файлов не остаётся
CONFIG="$RUN_DIR/config.memory.toml"
sed "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|" \
  ops/config.memory.toml > "$CONFIG"
MOCK_PID=""; SRV_PID=""

say()  { printf '\033[1;34m[e2e]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e FAIL]\033[0m %s\n' "$*"; exit 1; }

cleanup() { # только СВОИ процессы (pid-scoped): параллельные стенды не трогаем
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
}
trap cleanup EXIT

mcp() { # MCP-вызов на локальный сервер
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}
task_status() { # статус seed-строки через task_get
  mcp task_get "{\"number\":$SEED_N}" | python3 -c "
import json,sys
t = json.load(sys.stdin)['result']['content'][0]['text']
print(t.split(chr(10))[0].split('— ')[-1])" 2>/dev/null
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
rm -f "$STATE"

say "фаза 1: observe не заряжает счётчики петель"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 \
  MOCK_VERDICTS="result:request_changes+die" \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  ./target/debug/uwuwu-server >"$LOG_SRV_OBS" 2>&1 &
SRV_PID=$!
sleep 25   # 2+ тика (poll 10с)
kill "$SRV_PID" 2>/dev/null || true; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""
sleep 1
python3 - "$STATE" <<'PY'
import json, sys
st = json.load(open(sys.argv[1]))
loops = st.get("spawn_loops") or {}
assert not loops, f"observe зарядил счётчики петель: {loops}"
print(f"[e2e] state чист: spawn_loops={loops}")
PY
say "фаза 1 ок: observe без зарядки счётчиков"

say "фаза 2: live-цикл с ре-энтри (result request_changes+die → мёртвая dev-ячейка → respawn)"
rm -f "$STATE"
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" \
  RUST_LOG=info ./target/debug/uwuwu-server >"$LOG_SRV_LIVE" 2>&1 &
SRV_PID=$!
# сценарий вердиктов: первое ревью результата — на доработку (с мёртвой
# dev-ячейкой), дальше approve
SEEN_OK=0
while [ $SEEN_OK -lt 60 ]; do   # до 5 минут
  SEEN_OK=$((SEEN_OK + 1))
  sleep 5
  S="$(task_status || true)"
  case "$S" in
    *Review*) say "цикл замкнут: $S"; break ;;
    *attention*|*Needs*) fail "строка в $S (луп-гвард эскалировал?)" ;;
  esac
done
[ "${S:-}" = "" ] && fail "seed-строка не читается (сервер жив? лог: $LOG_SRV_LIVE)"
case "$S" in *Review*) ;; *) fail "цикл не замкнут за 5 мин, статус: $S";; esac

grep -q "loop_escalated" "$LOG_SRV_LIVE" && fail "loop_escalated в live-логе"
# ре-энти был: два dev-спавна (круг 1 + после request_changes)
DEV_SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N',"slot":"Dev"' "$LOG_SRV_LIVE" | wc -l)
[ "$DEV_SPAWNS" -ge 2 ] || fail "ре-энтри не случился (dev-спавнов: $DEV_SPAWNS)"
say "фаза 2 ок: ре-энтри круг пройден (dev-спавнов: $DEV_SPAWNS), эскалаций нет"

say "фаза 3: self-эхо — свой MCP-коммент не будит держащую сессию (#1204)"
# Гасим СВОИ сервер и быстрый мок фаз 1-2 (pid-scoped: параллельные
# стенды соседних сессий не трогаем) и поднимаем медленный мок
# (акт 11с при тике 10с) — каждый свежий сеанс на первом wake-тике видит
# хвост из коммента предшественника (создан раньше его активности):
# own-тест срезает тихо; до фикса тот же тик доставлял эхо (лишний акт).
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=11000 \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" \
  RUST_LOG=info ./target/debug/uwuwu-server >"$LOG_SRV_ECHO" 2>&1 &
SRV_PID=$!
# Цикл Design → … → Review на дефолтных approve с лагом актов: спавны
# стадий идут сменой статуса, собственные комменты каждой стадии срезаются
# на первом wake-тике её преемника (wakeup.self_echo_sliced).
SEEN_OK=0
while [ $SEEN_OK -lt 72 ]; do
  SEEN_OK=$((SEEN_OK + 1))
  sleep 5
  S="$(task_status || true)"
  case "$S" in
    *Review*) say "цикл замкнут: $S"; break ;;
    *attention*|*Needs*) fail "строка в $S" ;;
  esac
done
case "$S" in *Review*) ;; *) fail "цикл не замкнут за 6 мин, статус: $S";; esac
sleep 12   # окно эха: тик после idle-фазы последних актов
DELIVERED=$(grep -c '"event":"wakeup.delivered"' "$LOG_SRV_ECHO" || true)
SLICED=$(grep -c '"event":"wakeup.self_echo_sliced"' "$LOG_SRV_ECHO" || true)
[ "$SLICED" -ge 1 ] || fail "own-срез не сработал ни разу (sliced: $SLICED)"
[ "$DELIVERED" -eq 0 ] || fail "wake-доставок: $DELIVERED (ожидался 0 — эхо?)"
say "фаза 3 ок: sliced=$SLICED, delivered=0 — свои комменты не будят"

say "ИТОГ: E2E зелёный (observe-чистота + полный цикл с ре-энтри + self-эхо); логи: $RUN_DIR"
