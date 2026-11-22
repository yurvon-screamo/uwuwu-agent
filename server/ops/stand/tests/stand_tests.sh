#!/usr/bin/env bash
# stand_tests.sh — тест-сьют порт-контракта стендов (#1304).
#
# Что пинит (поведение, не строки):
#   1. stand_pick_ports: N различных свободных портов; отказ без python3 —
#      ненулевой RC; вызов без аргумента — громкий отказ.
#   2. Каждый стенд-скрипт при отказе подбора умирает на присваивании —
#      до cargo build и до старта процессов. Ассерт — наблюдаемая точка
#      смерти: rc≠0 + отказ python3 в stderr + say-прогресса в stdout нет
#      (подбор идёт до первой say-строки каждого скрипта). Shim несёт утилиты
#      шапки (dirname/mktemp/mkdir/sed/date) без python3; bash — абсолютным
#      путём (env резолвит «bash» по НОВОМУ PATH — иначе скрипт не стартует
#      и проба вырождается в вечный rc=127). Регресс «статус подбора
#      проглочен» (read <<<"$(…)") и снос вызова подбора красят пробу:
#      первый — say печатается, второй — python3 в stderr не появляется.
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

# PATH-шим: утилиты, которые скрипты зовут в шапке до подбора и между
# подбором и первой say-строкой; python3 НЕТ — единственная точка отказа
# здорового скрипта: присваивание ports="$(stand_pick_ports …)" под set -e
SHIM="$(mktemp -d)"
OUT_DIR="$(mktemp -d)"
trap 'rm -rf "$SHIM" "$OUT_DIR"' EXIT
for util in dirname mktemp mkdir sed date; do
  ln -s "$(command -v "$util")" "$SHIM/$util"
done

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
# bash — абсолютным путём: env резолвит аргумент по новому PATH только если
# он относительный, абсолютный путь исполняется как есть — скрипт реально
# стартует. Здоровый скрипт умирает на подборе: rc≠0, в stderr отказ
# python3, say-прогресса в stdout нет (подбор до первой say-строки).
BASH_BIN="$(command -v bash)"
for f in "${SCRIPTS[@]}"; do
  timeout 15 env PATH="$SHIM" "$BASH_BIN" "$STAND_DIR/$f" \
    >"$OUT_DIR/$f.out" 2>"$OUT_DIR/$f.err"
  rc=$?
  if [ "$rc" -ne 0 ] \
     && grep -Eqi 'python3.*(not found|no such file)' "$OUT_DIR/$f.err" \
     && ! grep -qF '[e2e' "$OUT_DIR/$f.out"; then
    ok "$f: отказ подбора роняет прогон на присваивании (rc=$rc, python3 отсутствует, say-прогресса нет)"
  else
    bad "$f: смерть не на подборе портов (rc=$rc; хвост stderr: $(tail -n 1 "$OUT_DIR/$f.err"); say-строк в stdout: $(grep -cF '[e2e' "$OUT_DIR/$f.out"))"
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
