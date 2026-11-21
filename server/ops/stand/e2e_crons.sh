#!/usr/bin/env bash
# e2e_crons.sh — мок-стенд крон-контура на плоском носителе (#1214):
# cron/<имя>/README.md (fm: title, schedule, agent, project) → sidecar файрит
# ран по расписанию → журнальная строка в cron/<имя>/Журнал.
#
# Полный цикл «мастер на новом пути → файр → журнал» без GitHub/LLM:
#   mock_opencode + uwuwu-server (memory hub); порты — подбор/оверрайд,
#   см. блок портов ниже.
#
# Ключевые несчастья того же контура:
#   fm без project → cron.master_invalid (мастер невидим, не файрится);
#   fm project вне карты [[project]] → cron.project_unknown (skip+warn);
#   cron_list: инвентарь + фильтр project, пустой фильтр — внятно пусто.
#
# Запуск: server/ops/stand/e2e_crons.sh   (из каталога server; cargo build сам)
# Выход: 0 — ок; 1 — провал с диагнозом. Логи: /tmp/uwuwu-stand-crons-*.log
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
# Порты: оверрайды E2E_CRON_PORT_* заданы — как есть; не заданы — свободные
# из пробы-бинда (гонка параллельных прогонов снята классом, #1304)
source "$(dirname "$0")/stand_ports.sh"
read -r PORT_SRV_D PORT_MOCK_D <<<"$(stand_pick_ports 2)"
PORT_MOCK="${E2E_CRON_PORT_MOCK:-$PORT_MOCK_D}"; PORT_SRV="${E2E_CRON_PORT_SRV:-$PORT_SRV_D}"
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
STATE=ops/state.json

LOG_MOCK=/tmp/uwuwu-stand-crons-$PORT_MOCK-mock.log
LOG_SRV=/tmp/uwuwu-stand-crons-$PORT_SRV-server.log

say()  { printf '\033[1;34m[e2e-crons]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e-crons FAIL]\033[0m %s\n' "$*"; exit 1; }

SRV_PID=""; MOCK_PID=""
cleanup() { for pid in "$SRV_PID" "$MOCK_PID"; do
              [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
            done; rm -f ops/config.memory.e2e-cron.toml "$STATE"; }
trap cleanup EXIT

mcp() { # MCP-вызов на живой сервер: mcp <tool> [json-args]
  local tool="$1"
  local args="${2:-}"
  [ -z "$args" ] && args='{}'
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT_SRV/mcp" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}"
}

say "сборка…"
cargo build -q --bin uwuwu-server --bin mock_opencode
rm -f "$STATE"

sed -e "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|" \
    ops/config.memory.toml > ops/config.memory.e2e-cron.toml

# сид вики: валидный мастер (project: uwuwu — есть в [[project]] memory-конфига),
# мастер с project вне карты, мастер с битым fm (нет project)
python3 - /tmp/uwuwu-stand-crons-seed.json <<'PY'
import json, sys
seed = {
 "cron/Проба-крон/README": "---\ntitle: Проба-крон\nschedule: 1m\nagent: tester\nproject: uwuwu\n---\n\nИнструкция рана: допиши итог одной строкой в журнал.\n",
 "cron/Чужой/README": "---\ntitle: Чужой крон\nschedule: 1m\nagent: tester\nproject: foilcap\n---\n\nproject вне карты [[project]] конфига стенда.\n",
 "cron/Битый/README": "---\ntitle: Битый крон\nschedule: 1m\n---\n\nfm без project — мастер невалиден.\n",
}
open(sys.argv[1], "w", encoding="utf-8").write(json.dumps(seed, ensure_ascii=False))
PY
UWUWU_MOCK_DOCS=$(cat /tmp/uwuwu-stand-crons-seed.json)
export UWUWU_MOCK_DOCS

say "стенд: mock_opencode ($PORT_MOCK) + сервер ($PORT_SRV, live, memory-hub)"
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=200 \
  MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV/mcp" \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.e2e-cron.toml \
PORT=$PORT_SRV UWUWU_SERVER_MODE=live UWUWU_SERVER_BEARER="$BEARER" \
UWUWU_OPENCODE_PASSWORD=mockpass UWUWU_SERVER_STATE="$STATE" \
RUST_LOG=info ./target/debug/uwuwu-server >"$LOG_SRV" 2>&1 &
SRV_PID=$!
sleep 2

# ---------- cron_list: инвентарь /cron ----------
say "инвентарь cron_list…"
LIST=$(mcp cron_list)
echo "$LIST" | grep -q 'Проба-крон \[uwuwu\]' || fail "cron_list не видит валидный мастер: $LIST"
echo "$LIST" | grep -q 'Чужой крон \[foilcap\]' || fail "cron_list не видит мастера с чужим project: $LIST"
echo "$LIST" | grep -q 'Битый' && fail "битый fm попал в cron_list: $LIST"
say "инвентарь: валидные мастера видны, битый fm отфильтрован"

say "фильтр cron_list project=uwuwu…"
FILTER=$(mcp cron_list '{"project":"uwuwu"}')
echo "$FILTER" | grep -q 'Проба-крон' || fail "фильтр uwuwu потерял Проба-крона: $FILTER"
echo "$FILTER" | grep -q 'Чужой' && fail "фильтр uwuwu протянул чужой project: $FILTER"
EMPTY=$(mcp cron_list '{"project":"origa"}')
echo "$EMPTY" | grep -q 'не найдены' || fail "пустой фильтр не внятно пуст: $EMPTY"
say "фильтр project работает: свой — есть, чужой — нет, пустой — внятно пуст"

# ---------- файр по расписанию (1m) + журнальная строка ----------
say "ожидание cron.fired (schedule 1m, до 100с)…"
FIRED=""
for _ in $(seq 1 50); do
  sleep 2
  FIRED=$(grep -o '"event":"cron.fired","cron":"cron/Проба-крон/README"[^}]*' "$LOG_SRV" | head -1)
  [ -n "$FIRED" ] && break
done
[ -n "$FIRED" ] || fail "крон не файрнулся за 100с (лог: $LOG_SRV)"
say "файр: $FIRED"

sleep 2
JOURNAL=$(mcp docs_read '{"slug":"cron/Проба-крон/Журнал"}')
echo "$JOURNAL" | grep -q 'запуск (демон)' || fail "журнальная строка не записана на новый путь: $JOURNAL"
echo "$JOURNAL" | grep -q '2026-..-..T..:..:..+09:00' || fail "штамп журнала не в JST: $JOURNAL"
say "журнал на cron/Проба-крон/Журнал: строка запуска с JST-штампом"

# ---------- несчастья контура ----------
grep -q '"event":"cron.project_unknown","cron":"cron/Чужой/README","project":"foilcap"' "$LOG_SRV" \
  || fail "project вне карты — нет warn cron.project_unknown (skip+warn)"
NOJ=$(mcp docs_read '{"slug":"cron/Чужой/Журнал"}')
echo "$NOJ" | grep -q '"isError":true' || fail "у крона вне карты появился журнал — он файрится?!: $NOJ"
say "project вне карты: skip+warn, файра и журнала нет"

grep -q '"event":"cron.master_invalid","slug":"cron/Битый/README"' "$LOG_SRV" \
  || fail "битый fm — нет warn cron.master_invalid"
say "битый fm: skip+warn, мастер невидим"

say "ИТОГ: E2E кронов зелёный (мастер → файр → журнал; фильтры; битые fm)"
