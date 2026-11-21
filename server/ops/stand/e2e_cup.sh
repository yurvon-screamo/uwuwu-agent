#!/usr/bin/env bash
# e2e_cup.sh — живой прогон e2e-матрицы CUP-капа (#1313) на мок-стенде
# (без GitHub/LLM, паттерн e2e_peak.sh #1310). База — reachable-семантика
# деградации #1450 (PR #475, смержен 21.11).
#
# Сценарии:
#   V1 busy-подсчёт: N busy-сессий держат кап, кандидаты held_cup,
#     выезд по мере освобождения слотов;
#   V2 смешанное здоровье: исполнитель B вниз — конвейер жив, кап считает
#     reachable-busy живого A (#1450), строки A не морозятся;
#   V3 восстановление: B возвращается — его очередь выезжает
#     (слепой hold #1263 снимается);
#   V4 зомби-флаг: busy>idle старше 30 мин (#1295) не держит слот;
#   V5 мягкий сталл-пинг: при полном капе держится (#1313),
#     на свободном слоте доставляется.
#
# Запуск: server/ops/stand/e2e_cup.sh   (из каталога server; cargo build сам)
# Порты — 8299/8298 (моки A/B) и 8290 (сервер), #1304/#1328.
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_A="${PORT_MOCK_A:-8299}"; PORT_B="${PORT_MOCK_B:-8298}"; PORT_SRV="${PORT_SRV:-8290}"
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
export GH_TOKEN="${GH_TOKEN:-stand-e2e-dummy}"
RUN_DIR="${UWUWU_STAND_RUN_DIR:-$(mktemp -d /tmp/uwuwu-stand-cup-XXXXXX)}"
mkdir -p "$RUN_DIR"
LOG_A="$RUN_DIR/mock_a.log"; LOG_B="$RUN_DIR/mock_b.log"; LOG_SRV="$RUN_DIR/server.log"
STATE="$RUN_DIR/state.json"
CONFIG="$RUN_DIR/config.cup.toml"
PID_A=""; PID_B=""; PID_B2=""; SRV_PID=""; KEEP=0
MARKS=/tmp/opencode/cup_marks.txt
: > "$MARKS"
mark() { echo "$(date -u +%H:%M:%S) $*" >> "$MARKS"; }
say()  { printf '\033[1;34m[e2e-cup]\033[0m %s\n' "$*"; mark "say: $*"; }
fail() { printf '\033[1;31m[e2e-cup FAIL]\033[0m %s\n' "$*"; echo "$(date -u +%H:%M:%S) FAIL: $*" >> "$MARKS"; KEEP=1; exit 1; }
# диагностика киллера (#1313): сигналы скрипту — в метки, не смерть
trap 'mark "SIGNAL caught (TERM/INT/HUP)"' TERM INT HUP

cleanup() {
  echo "$(date -u +%H:%M:%S) cleanup rc=${KEEP} srv=$SRV_PID" >> "$MARKS"
  for pid in "$SRV_PID" "$PID_A" "$PID_B" "$PID_B2"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  [ "${BIN_SRV:-}" ] && rm -f "$BIN_SRV" "$BIN_MOCK"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$RUN_DIR"
}
trap cleanup EXIT

mk_config() { # $1 — файл
  cat > "$1" <<EOF
mode = "live"
poll_sec = 10
port = $PORT_SRV

[hub]
repo = "yurvon-screamo/uwuwu-hub"
driver = "memory"

[executor]
name = "kisa-mock"
url = "http://127.0.0.1:$PORT_A"

[[executors]]
name = "redmi-mock"
url = "http://127.0.0.1:$PORT_B"

[policy]
cup = 2
peak = { enabled = false, start_min = 0, end_min = 0 }

[[project]]
name = "uwuwu"
board = "Задачи uwuwu"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"

[[project]]
name = "p2"
board = "Доска p2"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"
executor = "redmi-mock"
EOF
}
mk_config "$CONFIG"
SRV_PID=""

# Ожидание фазы с присмотром за сервером: смерть uwuwu-server в окне
# ожидания — громкий FAIL с диагнозом, не тихий срыв сценария (#1313).
wait_phase() { # $1 — секунд
  local i
  for ((i = 0; i < $1; i += 5)); do
    sleep 5
    if ! kill -0 "$SRV_PID" 2>/dev/null; then
      mark "SRV DEAD на +$((i + 5)) c — хвост server.log:"
      tail -30 "$LOG_SRV" >> "$MARKS" 2>&1
      fail "uwuwu-server ($SRV_PID) умер на +$((i + 5)) c фазы; лог: $LOG_SRV"
    fi
  done
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
BIN_SRV="$RUN_DIR/uwuwu-server"; BIN_MOCK="$RUN_DIR/mock_opencode"
cp target/debug/uwuwu-server "$BIN_SRV"; cp target/debug/mock_opencode "$BIN_MOCK"
rm -f "$STATE"

start_mock() { # $1 порт, $2 лог, $3 mode — ждём поднятия порта до 15 с
  MOCK_MODE="$3" MOCK_PORT="$1" MOCK_STEP_MS=300 "$BIN_MOCK" >"$2" 2>&1 &
  echo $!
  local ok=""
  for _ in $(seq 1 30); do
    if curl -s --max-time 2 -o /dev/null "http://127.0.0.1:$1/api/model"; then ok=1; break; fi
    sleep 0.5
  done
  [ -n "$ok" ] || fail "мок на порту $1 не поднялся за 15 с (лог: $2)"
}
stop_pid() { kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }
mk_busy() { # $1 порт, $2 ms_ago — свежий/протухший busy; ретрай ×2
  local id=""
  for _ in 1 2; do
    id=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$1/api/session" \
      -H 'Content-Type: application/json' \
      -d "{\"title\":\"busy-filler\",\"location\":{\"directory\":\"/tmp\"},\"busy_ms_ago\":$2}" \
      | grep -oE 'ses_mock[0-9a-f]+' | head -1 || true)
    [ -n "$id" ] && break
    sleep 2
  done
  echo "$id"
}

# ===== V1: busy-подсчёт — кап полон, выезд по освобождению =====
say "V1: busy×2 держат кап=2, кандидат held_cup; освобождение → spawn"
PID_A=$(start_mock "$PORT_A" "$LOG_A" "plain")
PID_B=$(start_mock "$PORT_B" "$LOG_B" "plain")
sleep 1
B1=$(mk_busy "$PORT_A" 0); B2=$(mk_busy "$PORT_A" 0)
[ -n "$B1" ] && [ -n "$B2" ] || fail "филлеры busy не созданы"
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=x \
  UWUWU_MOCK_SEED="Задачи uwuwu:3300:Design:Medium;Задачи uwuwu:3301:Design:Medium" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV" 2>&1 &
SRV_PID=$!
wait_phase 25
grep -q '"event":"spawn.held_cup"' "$LOG_SRV" || fail "V1: held_cup нет — кап не считает busy"
H1=$(grep -oE '"event":"spawn.done","number":330[01]' "$LOG_SRV" | wc -l || true)
[ "$H1" -eq 0 ] || fail "V1: кандидат заспавнен при полном капе ($H1)"
say "V1: кап полон — held_cup жив (busy-филлеры не дали слота)"
# освобождение слота — кандидат выезжает следующим тиком
curl -s --max-time 5 -X DELETE "http://127.0.0.1:$PORT_A/api/session/$B1" >/dev/null
wait_phase 25
D1=$(grep -oE '"event":"spawn.done","number":330[01]' "$LOG_SRV" | wc -l || true)
[ "$D1" -ge 1 ] || fail "V1: после освобождения слота кандидат не выехал"
say "V1 ок: busy держит кап, освобождение выпускает кандидата"
stop_pid "$SRV_PID"; SRV_PID=""

# ===== V2: смешанное здоровье — reachable-кап, живой исполнитель спавнит =====
say "V2: B вниз, A жив с busy×2 — кап по reachable, строки A не морозятся"
stop_pid "$PID_B"; PID_B=""
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=x \
  UWUWU_MOCK_SEED="Задачи uwuwu:3300:Design:Medium;Задачи uwuwu:3301:Design:Medium;Доска p2:3400:Design:Medium" \
  RUST_LOG=info "$BIN_SRV" >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
wait_phase 25
grep -q '"event":"spawn.held_cup"' "$LOG_SRV" || fail "V2: reachable-кап не держит кандидата A (busy×2 ≥ cup)"
P2_BLIND=$(grep -c '"event":"spawn.held_blind_scan"' "$LOG_SRV" || true)
[ "$P2_BLIND" -ge 1 ] || fail "V2: строка p2 не держится слепым hold (#1263)"
say "V2 ок: A-кандидаты held_cup по reachable-busy, p2 — blind hold; конвейер жив (tick.completed есть)"
grep -q '"event":"tick.completed"' "$LOG_SRV" || fail "V2: тик умер на деградации"

# ===== V3: восстановление B — его очередь выезжает =====
say "V3: B возвращается — строка p2 3400 спавнится"
PID_B2=$(start_mock "$PORT_B" "$LOG_B" "plain")
wait_phase 25
P2_SPAWN=$(grep -oE '"event":"spawn.done","number":3400' "$LOG_SRV" | wc -l || true)
[ "$P2_SPAWN" -ge 1 ] || fail "V3: строка p2 3400 не выехала после восстановления B"
say "V3 ок: восстановление исполнителя выпускает его очередь"

# ===== V4: зомби-флаг не держит слот =====
say "V4: busy-флаг старше 30 мин (#1295) — слот свободен, кандидат едет"
stop_pid "$SRV_PID"; SRV_PID=""
rm -f "$STATE"
Z=$(mk_busy "$PORT_A" 2100000)   # 35 мин назад — протухший busy
[ -n "$Z" ] || fail "V4: зомби-филлер не создан — сценарий недействителен"
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=x \
  UWUWU_MOCK_SEED="Задачи uwuwu:3300:Design:Medium" \
  RUST_LOG=info "$BIN_SRV" >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
wait_phase 25
Z_SPAWN=$(grep -oE '"event":"spawn.done","number":3300' "$LOG_SRV" | wc -l || true)
[ "$Z_SPAWN" -ge 1 ] || fail "V4: протухший busy держит слот — кандидат 3300 стоит"
say "V4 ок: зомби-флаг кап не держит, кандидат выехал"

# ===== V5: мягкий сталл-пинг — гейт капом (#1313) =====
say "V5: кап полон — мягкий пинг держится (stall.soft_ping_held_cup); слот освободился → доставлен"
stop_pid "$SRV_PID"; SRV_PID=""
rm -f "$STATE"
# кандидаты: 3300 (dev-спавн → stall-носитель), 3301; кап полон филлерами
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=x \
  UWUWU_MOCK_SEED="Задачи uwuwu:3300:Design:Medium;Задачи uwuwu:3301:Design:Medium" \
  RUST_LOG=info "$BIN_SRV" >>"$LOG_SRV" 2>&1 &
SRV_PID=$!
sleep 25   # тик: спавны 3300/3301 (слоты свободны после V4 — busy филлер Z протух)
# кап полон: два свежих busy-филлера
F1=$(mk_busy "$PORT_A" 0); F2=$(mk_busy "$PORT_A" 0)
[ -n "$F1" ] && [ -n "$F2" ] || fail "V5: филлеры не созданы"
# носитель пинга: строка 3300 со спавненной dev-сессией, тишина 16 мин
DEV3300=$(grep -oE '"event":"spawn.done","number":3300[^}]*"session":"ses_mock[0-9a-f]+' "$LOG_SRV" | grep -oE 'ses_mock[0-9a-f]+' | tail -1 || true)
[ -n "$DEV3300" ] || fail "V5: dev-сессия 3300 не найдена в логе"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_A/api/session/$DEV3300/age" \
  -H 'Content-Type: application/json' -d "{\"ms\": 960000, \"messages\": true}" >/dev/null
sleep 25   # тик: сталл-проход 3300 — пинг держится капом
grep -q '"event":"stall.soft_ping_held_cup","number":3300' "$LOG_SRV" \
  || fail "V5: мягкий пинг не держится полным капом"
grep -q '"event":"stall.soft_pinged"' "$LOG_SRV" \
  && fail "V5: пинг доставлен вопреки полному капу"
say "V5: пинг держится (held), доставки нет"
# слот освобождается: гасим ВСЕ busy-филлеры — B2 из V1 тоже fresh-busy
# и держит кап вместе с F2 (мок A копит сессии между фазами)
for filler in "$F1" "$F2" "$B2"; do
  curl -s --max-time 5 -X DELETE "http://127.0.0.1:$PORT_A/api/session/$filler" >/dev/null
done
sleep 1
wait_phase 25
grep -q '"event":"stall.soft_pinged"' "$LOG_SRV" \
  || fail "V5: после освобождения слота пинг не доставлен"
grep -q 'loop_escalated' "$LOG_SRV" \
  && fail "V5: луп-гвард эскалировал — холд пинга не должен заряжать гарды"
say "V5 ок: холд без mark, доставка на свободном слоте"
say "ИТОГ: V1–V5 зелёные — CUP-матрица живьём"
