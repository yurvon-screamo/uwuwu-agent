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
#   4. Судейская пара фазы 15 (e2e.sh) на контракте портов, не на
#      деривации (#1304, диспозиция #1364): дефолтный путь порт-блока
#      даёт 5 различных портов, PORT_JUDGE/PORT_JUDGE_DOWN живут
#      независимыми переменными (регресс-форма мастера a1e33159 —
#      PORT_JUDGE="${PORT_JUDGE:-$((PORT_MOCK + 4))}" без присваивания
#      PORT_JUDGE_DOWN — громко умирает под set -u: переменной нет);
#      оверрайды уважаются как есть.
#      Граница поведенческих проб 4a/4b: форма «деривация с обеими
#      присвоенными переменными» (PORT_JUDGE=…+4, PORT_JUDGE_DOWN=…+1)
#      жива и различна — для них зелёная; её ловит структурный пин 4c —
#      арифметики в извлечённом порт-блоке нет (контракт: порт — оверрайд
#      или подбор, никаких вычислений).
#      Красная сторона проверена деградат-копией e2e.sh с линиями
#      a1e33159 (см. раунд-отчёт тестирования 23.11).
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

ports="$(stand_pick_ports 5)"
read -r J1 J2 J3 J4 J5 <<<"$ports"
if [ "$(printf '%s\n' "$J1" "$J2" "$J3" "$J4" "$J5" | sort -u | wc -l)" = 5 ] \
   && [ "$J1" -gt 0 ] && [ "$J2" -gt 0 ] && [ "$J3" -gt 0 ] && [ "$J4" -gt 0 ] && [ "$J5" -gt 0 ]; then
  ok "stand_pick_ports 5 → 5 различных свободных портов ($J1 $J2 $J3 $J4 $J5)"
else
  bad "stand_pick_ports 5 → '$ports' (ожидались 5 различных положительных)"
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

# ---- 4. судейская пара фазы 15: порт-блок e2e.sh на контракте -------------
# Порт-блок e2e.sh (подбор + присваивания до PORT_JUDGE_DOWN включительно)
# исполняется в песочнице как есть — поведение, не строки:
#   4a. дефолтный путь (без оверрайдов): все 5 переменных живы и различны —
#       судейская пара кормится подбором; регресс-форма мастера a1e33159
#       (PORT_JUDGE деривацией, PORT_JUDGE_DOWN нет) умирает под set -u на
#       первом обращении к PORT_JUDGE_DOWN — громко, до процессов.
#   4b. оверрайды PORT_JUDGE/PORT_JUDGE_DOWN (как и три старых) берутся
#       как есть — подбор их не перетирает.
PORT_BLOCK="$(awk '/^ports="\$\(stand_pick_ports/{f=1} f{print} /^PORT_JUDGE="/{exit}' "$STAND_DIR/e2e.sh")"

run_port_block() { # env-оверрайды зовущего проходят в подшеллку как есть
  (
    set -euo pipefail
    # shellcheck disable=SC1091
    source "$STAND_DIR/stand_ports.sh"
    eval "$PORT_BLOCK"
    # shellcheck disable=SC2153 # переменные присваивает eval порт-блока — shellcheck не видит
    alls="$PORT_SRV $PORT_MOCK $MOCK2_PORT $PORT_JUDGE $PORT_JUDGE_DOWN"
    # shellcheck disable=SC2086 # $alls режется на 5 портов намеренно (sort -u построчно)
    [ "$(printf '%s\n' $alls | sort -u | wc -l)" = 5 ] \
      && [ "$PORT_SRV" -gt 0 ] && [ "$PORT_JUDGE_DOWN" -gt 0 ] \
      && echo "$alls"
  )
}

if run_port_block; then
  ok "e2e.sh порт-блок (дефолт): 5 переменных живы и различны — судейская пара на подборе"
else
  bad "e2e.sh порт-блок (дефолт): судейская пара не сошлась из подбора (см. вывод выше)"
fi

if out5="$(PORT_SRV=18101 PORT_MOCK=18102 MOCK2_PORT=18103 PORT_JUDGE=18104 PORT_JUDGE_DOWN=18105 run_port_block 2>&1)" \
   && [ "$out5" = "18101 18102 18103 18104 18105" ]; then
  ok "e2e.sh порт-блок: все 5 оверрайдов взяты как есть ($out5)"
else
  bad "e2e.sh порт-блок: оверрайды не уважены (вышло: '$(PORT_SRV=18101 PORT_MOCK=18102 MOCK2_PORT=18103 PORT_JUDGE=18104 PORT_JUDGE_DOWN=18105 run_port_block 2>&1 | tail -1)')"
fi

#   4c. структурный пин: арифметики «((» в извлечённом порт-блоке нет —
#       любая форма деривации (включая «обе переменные присвоены», которую
#       4a/4b поведенчески не различают) красит по построению: контрактом
#       порт — оверрайд или подбор, вычислений в блоке быть не должно.
if grep -qF '((' <<<"$PORT_BLOCK"; then
  bad "e2e.sh порт-блок: арифметика деривации в блоке — снесённый класс болезни вернулся"
else
  ok "e2e.sh порт-блок: арифметики деривации нет (структурный пин)"
fi

finish
