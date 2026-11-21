#!/usr/bin/env bash
# stand_tests.sh — тест-сьют порт-контракта стендов (#1304).
#
# Что пинит (поведение, не строки):
#   1. stand_pick_ports: N различных свободных портов; отказ без python3 —
#      ненулевой RC; вызов без аргумента — громкий отказ.
#   2. Каждый стенд-скрипт при отказе подбора умирает на присваивании —
#      до cargo build и до старта процессов (паттерн ports="$(...)" под
#      set -e; регресс-пин Common-1 ревью #1304; снос вызова подбора
#      скриптом тоже красит эту пробу — скрипт перестаёт падать).
#   3. mock.started строго после успешного бинда с фактическим портом:
#      занятый порт → RC≠0 и «started» в логе НЕТ (маскировка «up + паника»
#      не вернулась); свободный → строка есть, requested_port == port.
#
# Запуск: bash server/ops/stand/tests/stand_tests.sh   (cargo build мока сам)
# Выход: 0 — все пробы зелёные; 1 — есть провалы (список в stdout).
set -uo pipefail

STAND_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SERVER_DIR="$STAND_DIR/../.."
SCRIPTS=(e2e.sh e2e_bots.sh e2e_crons.sh e2e_peak.sh e2e_cup.sh)

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "ok: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "FAIL: $1"; }
finish() {
  echo "----"
  echo "итог: $PASS ok, $FAIL FAIL"
  [ "$FAIL" -eq 0 ]
}

# PATH-шим без python3: только то, что скрипты зовут до подбора портов
SHIM="$(mktemp -d)"
trap 'rm -rf "$SHIM"' EXIT
ln -s "$(command -v dirname)" "$SHIM/dirname"

# ---- 1. юнит: stand_pick_ports -------------------------------------------
# shellcheck disable=SC1091
source "$STAND_DIR/stand_ports.sh"

ports="$(stand_pick_ports 3)"
read -r P1 P2 P3 <<<"$ports"
if [ -n "$P1" ] && [ -n "$P2" ] && [ -n "$P3" ] && [ "$P1" != "$P2" ] && [ "$P2" != "$P3" ] && [ "$P1" != "$P3" ] \
   && [ "$P1" -gt 0 ] && [ "$P2" -gt 0 ] && [ "$P3" -gt 0 ]; then
  ok "stand_pick_ports 3 → 3 различных свободных порта ($P1 $P2 $P3)"
else
  bad "stand_pick_ports 3 → '$ports' (ожидались 3 различных положительных)"
fi

if ( PATH="$SHIM" stand_pick_ports 1 ) 2>/dev/null; then
  bad "stand_pick_ports без python3 → RC=0 (ожидался ненулевой)"
else
  ok "stand_pick_ports без python3 → громкий отказ"
fi

if ( stand_pick_ports ) 2>/dev/null; then
  bad "stand_pick_ports без аргумента → RC=0 (ожидался ненулевой)"
else
  ok "stand_pick_ports без аргумента → громкий отказ"
fi

# ---- 2. негатив-пин: отказ подбора роняет каждый скрипт до процессов -----
# PATH без python3: подбор фейлится на присваивании (set -e), скрипт умирает
# мгновенно — не доходя до cargo build и старта серверов/моков.
for f in "${SCRIPTS[@]}"; do
  if timeout 15 env PATH="$SHIM" bash "$STAND_DIR/$f" >/dev/null 2>&1; then
    bad "$f: PATH без python3 → rc=0 за 15с — скрипт не умирает на присваивании \
(снесён вызов stand_pick_ports или проверка отказа)"
  else
    ok "$f: отказ подбора роняет прогон (rc≠0, до старта процессов)"
  fi
done

# ---- 3. интеграция: mock.started строго за биндом -------------------------
MOCK_BIN="$SERVER_DIR/target/debug/mock_opencode"
if [ ! -x "$MOCK_BIN" ]; then
  (cd "$SERVER_DIR" && cargo build -q --bin mock_opencode) || { bad "сборка mock_opencode"; finish; exit 1; }
fi

LOG="$(mktemp)"
free_pair="$(stand_pick_ports 2)"
read -r P_BUSY P_FREE <<<"$free_pair"

# слушатель занимает P_BUSY
python3 - "$P_BUSY" <<'PY' &
import socket, sys, time
s = socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(1)
print("ready", flush=True); time.sleep(60)
PY
OCC_PID=$!
# ждём готовности слушателя (строка ready)
for _ in $(seq 1 50); do kill -0 "$OCC_PID" 2>/dev/null || break; sleep 0.1; done

# 3a: занятый порт — смерть без «mock.started»
rm -f "$LOG"
MOCK_PORT="$P_BUSY" MOCK_MODE=plain "$MOCK_BIN" >"$LOG" 2>&1
rc=$?
if [ "$rc" -ne 0 ] && ! grep -q '"event":"mock.started"' "$LOG"; then
  ok "мок на занятом порту $P_BUSY: rc=$rc, mock.started в логе нет"
else
  bad "мок на занятом порту $P_BUSY: rc=$rc, started в логе $(grep -c 'mock.started' "$LOG") — маскировка вернулась"
fi

# 3b: свободный порт — started с фактическим портом, requested == actual
rm -f "$LOG"
MOCK_PORT="$P_FREE" MOCK_MODE=plain "$MOCK_BIN" >"$LOG" 2>&1 &
MOCK_PID=$!
up="no"
for _ in $(seq 1 100); do
  grep -q '"event":"mock.started"' "$LOG" && { up="yes"; break; }
  sleep 0.1
done
if [ "$up" = "yes" ] && grep -q "\"requested_port\":$P_FREE,\"port\":$P_FREE" "$LOG"; then
  ok "мок на свободном порту $P_FREE: mock.started после бинда, requested_port == port"
else
  bad "мок на свободном порту $P_FREE: up=$up, started-строка: $(grep -o '"event":"mock.started".*' "$LOG" | head -1)"
fi
kill "$MOCK_PID" 2>/dev/null
kill "$OCC_PID" 2>/dev/null

finish
