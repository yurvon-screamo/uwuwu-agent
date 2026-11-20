#!/usr/bin/env bash
# e2e_peak.sh — живой прогон сценария владельца (18.11, #1310):
# «пик-окно × ребут ноды × приоритеты» на мок-стенде (без GitHub/LLM).
#
# Фазы (чек-лист V1–V4 юнит-матрицы tick_tests.rs, живьём):
#   фаза 1 (пик): medium-кандидат отложен (spawn.deferred_peak), high
#     спавнится сквозь пик, busy-сессия medium в env не прерывается
#     (гард second source #1163 держит, abort-рута молчит);
#   фаза 2 (ребут ноды): исполнитель убит — тик живёт, спавны держатся
#     (spawn.held_blind_scan #1263); мок возвращается — high-строка
#     с мёртвой ячейкой респавнится (пик high не держит), medium
#     по-прежнему deferred;
#   фаза 3 (конец окна): рестарт uwuwu-server с peak off и ТЕМ ЖЕ
#     state.json — deferred-носителя нет (fact #1310: гейт пересчитывается
#     временем тика), medium выезжает на первом тике, потерь и эскалаций нет.
#
# Запуск: server/ops/stand/e2e_peak.sh   (из каталога server; cargo build сам)
# Порты — свои дефолты 8199/8190 (гонки с e2e.sh 8099/8090 исключены,
# #1304/#1328); пер-оверрайд: PORT_MOCK=X PORT_SRV=Y bash e2e_peak.sh
# Пик реален (weekday-гейт PeakWindow): прогон в будни; в выходные —
# громкий отказ с причиной, не тихий скип.
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_MOCK="${PORT_MOCK:-8199}"; PORT_SRV="${PORT_SRV:-8190}"
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
# config.secrets() валидирует GH_TOKEN безусловно (non_empty_env), хотя
# memory-driver в GitHub не ходит — дамми достаточно (основной e2e.sh
# молча полагается на GH_TOKEN окружения хоста; тут — явная ручка)
export GH_TOKEN="${GH_TOKEN:-stand-e2e-dummy}"
RUN_DIR="${UWUWU_STAND_RUN_DIR:-$(mktemp -d /tmp/uwuwu-stand-peak-XXXXXX)}"
mkdir -p "$RUN_DIR"
LOG_MOCK="$RUN_DIR/mock.log"
LOG_SRV="$RUN_DIR/server.log"
STATE="$RUN_DIR/state.json"
SEED_BUSY=2060   # medium In Progress: ход «в полёте» (сессия в env, #1163)
SEED_M=2061      # medium Design: спавн-кандидат — deferred_peak
SEED_H=2062      # high Design: спавн сквозь пик
CONFIG="$RUN_DIR/config.peak.toml"
CONFIG_OFF="$RUN_DIR/config.nopeak.toml"
MOCK_PID=""; SRV_PID=""

say()  { printf '\033[1;34m[e2e-peak]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e-peak FAIL]\033[0m %s\n' "$*"; KEEP=1; exit 1; }

cleanup() {
  for pid in "$SRV_PID" "$MOCK_PID"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  [ -n "${BIN_SRV:-}" ] && rm -f "$BIN_SRV" "$BIN_MOCK"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$RUN_DIR"
}
trap cleanup EXIT

mcp() {
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}

# Пик реален: PeakWindow держит только будни (weekday 1..=5). Выходной —
# громкий отказ с причиной (детерминизм прогона важнее автоскипа).
WD="$(date -u +%u)"
[ "$WD" -le 5 ] || fail "сегодня выходной (weekday=$WD): PeakWindow по канону не держит — прогон перенести на будни"

# Окно вокруг текущего времени UTC: держит фазы 1–2 (~3 мин), фаза 3 —
# рестарт сервера с peak off («конец окна» без ожидания).
START=$(( $(date -u +%-H) * 60 + 10#$(date -u +%-M) - 1 ))
END=$(( START + 40 ))
say "окно пика: будни, [${START}, ${END}) минут UTC (сейчас ~$((START+1)))"

cat > "$CONFIG" <<EOF
mode = "live"
poll_sec = 10
port = $PORT_SRV

[hub]
repo = "yurvon-screamo/uwuwu-hub"
driver = "memory"

[executor]
name = "local-mock"
url = "http://127.0.0.1:$PORT_MOCK"

[policy]
cup = 7
peak = { enabled = true, start_min = $START, end_min = $END }

[[project]]
name = "uwuwu"
board = "Задачи uwuwu"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"
EOF
# Конец окна: peak off (V3/V4 — гейт оценивается временем тика после рестарта)
sed 's/peak = { enabled = true.*}/peak = { enabled = false, start_min = 0, end_min = 0 }/' \
  "$CONFIG" > "$CONFIG_OFF"

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
BIN_SRV="$RUN_DIR/uwuwu-server"
BIN_MOCK="$RUN_DIR/mock_opencode"
cp target/debug/uwuwu-server "$BIN_SRV"
cp target/debug/mock_opencode "$BIN_MOCK"
rm -f "$STATE"

start_mock() { # busy: сессии с ходом в полёте (busy по union-скану, #1231)
  MOCK_MODE=busy MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
    "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
  MOCK_PID=$!
  sleep 1
}

say "фаза 1: пик держит medium-кандидат, high идут сквозь пик"
start_mock
# ход «в полёте»: живая сессия в env строки 2060 (second source #1163) —
# busy по union-скану; строка без ячейки — спавн держится гардом, не гейтом
ENV_DIR="/home/yurvon/agents-env/$SEED_BUSY"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session" \
  -H 'Content-Type: application/json' \
  -d "{\"title\":\"turn-2060\",\"location\":{\"directory\":\"$ENV_DIR\"}}" >/dev/null
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:$SEED_BUSY:In Progress:Medium;Задачи uwuwu:$SEED_M:Design:Medium;Задачи uwuwu:$SEED_H:Design:High" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV" 2>&1 &
SRV_PID=$!
sleep 25   # 2+ тика в пике
grep -q '"event":"spawn.deferred_peak","number":'$SEED_M "$LOG_SRV" \
  || fail "medium-кандидат $SEED_M не отложен пиком (spawn.deferred_peak нет)"
grep -q '"event":"spawn.done","number":'$SEED_H "$LOG_SRV" \
  || fail "high $SEED_H не прошёл сквозь пик"
M_SPAWN=$(grep -oE '"event":"spawn.done","number":'$SEED_M "$LOG_SRV" | wc -l || true)
[ "$M_SPAWN" -eq 0 ] || fail "medium $SEED_M заспавнен В ПИКЕ: $M_SPAWN"
grep -q '"event":"spawn.held_live_session"' "$LOG_SRV" \
  || fail "ход 2060 не держится гардом second source (#1163)"
grep -qa 'mock.session_aborted' "$LOG_MOCK" \
  && fail "abort-рута дёрнулась — пик прервал ход в полёте"
say "фаза 1 ок: 2061 deferred, 2062 спавнен, ход 2060 жив (held_live), абортов нет"

say "фаза 2: ребут ноды — исполнитель умирает вместе с сессиями"
kill "$MOCK_PID" 2>/dev/null || true; wait "$MOCK_PID" 2>/dev/null || true; MOCK_PID=""
sleep 25   # 2+ тика на мёртвом исполнителе
grep -q '"event":"spawn.held_blind_scan"' "$LOG_SRV" \
  || fail "на мёртвом исполнителе нет слепого hold (#1263): спавн вслепую?"
grep -q '"event":"tick.completed"' "$LOG_SRV" \
  || fail "тик умер на ребуте ноды"
say "ребут: тик живёт, спавны держатся (blind hold)"
start_mock
sleep 25   # тик после «включения ноды»
H_SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_H "$LOG_SRV" | wc -l || true)
[ "$H_SPAWNS" -ge 2 ] || fail "high $SEED_H не респавнился после ребута (ячеёка с мёртвой сессией): $H_SPAWNS"
grep -q '"event":"spawn.deferred_peak","number":'$SEED_M "$LOG_SRV" \
  || fail "medium $SEED_M после ребута не держится пиком"
say "фаза 2 ок: high респавнился после ребута ($H_SPAWNS спавнов), medium всё ещё в пике"

say "фаза 3: конец окна — рестарт сервера (peak off), state переживает рестарт"
kill "$SRV_PID" 2>/dev/null || true; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""
sleep 1
[ -f "$STATE" ] || fail "state.json не пережил рестарт (носитель потерян)"
UWUWU_SERVER_CONFIG=$CONFIG_OFF UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:$SEED_BUSY:In Progress:Medium;Задачи uwuwu:$SEED_M:Design:Medium;Задачи uwuwu:$SEED_H:Design:High" \
  RUST_LOG=info "$BIN_SRV" >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
sleep 25   # первый тик после конца окна
grep -q '"event":"spawn.done","number":'$SEED_M "$LOG_SRV" \
  || fail "medium $SEED_M не выехал после конца окна (отложенный спавн потерян?)"
say "фаза 3 ок: medium выехал на первом тике после окна (рестарт потерь не принёс)"
grep -q 'loop_escalated' "$LOG_SRV" \
  && fail "луп-гвард эскалировал — ложный сталл на deferred"
say "итог: сценарий владельца пережит живьём — без потери отложенных спавнов и без ложных сталлов"
