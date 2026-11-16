#!/usr/bin/env bash
# e2e.sh — автоматический E2E мок-стенда uwuwu-server (без GitHub/LLM).
#
# Покрывает инцидент #1046 (15.11) комплексно:
#   фаза 1 (observe-яд): строка In Progress с пустым слотом живёт в observe
#     2+ тика — счётчики спавн-петель НЕ заряжаются (state.json чист);
#   фаза 2 (live-цикл с ре-энтри): полный цикл Design → … → Review,
#     где ревью результата один раз даёт request_changes (In Progress →
#     повторный dev-круг → …) — луп-гвард не эскалирует, цикл замыкается.
#
# Запуск: server/ops/stand/e2e.sh   (из каталога server; cargo build сам)
# Выход: 0 — ок; 1 — провал с диагнозом. Логи: /tmp/uwuwu-stand-*.log
set -euo pipefail

cd "$(dirname "$0")/../.."   # server/
PORT_MOCK=8099; PORT_SRV=8090
BEARER="${UWUWU_SERVER_BEARER:-stand-e2e-bearer}"
SEED_N=2046
LOG_MOCK=/tmp/uwuwu-stand-mock.log
LOG_SRV_OBS=/tmp/uwuwu-stand-server-observe.log
LOG_SRV_LIVE=/tmp/uwuwu-stand-server-live.log
STATE=ops/state.json

say()  { printf '\033[1;34m[e2e]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e FAIL]\033[0m %s\n' "$*"; exit 1; }

cleanup() { pkill -f 'target/debug/uwuwu-server' 2>/dev/null || true
            pkill -f 'target/debug/mock_opencode' 2>/dev/null || true; }
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
sleep 1
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.toml UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  ./target/debug/uwuwu-server >"$LOG_SRV_OBS" 2>&1 &
sleep 25   # 2+ тика (poll 10с)
pkill -f 'target/debug/uwuwu-server' || true; sleep 1
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
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory.toml UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" \
  RUST_LOG=info ./target/debug/uwuwu-server >"$LOG_SRV_LIVE" 2>&1 &
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

say "фаза 3: мульти-экзекьютор — маршрутизация проект → executor + отказ исполнителя (#1203)"
# фаза 2 оставила свой стек (сервер 8090 + мок 8099) — чистим перед стендом
pkill -f 'target/debug/uwuwu-server' 2>/dev/null || true
pkill -f 'target/debug/mock_opencode' 2>/dev/null || true
sleep 1
rm -f "$STATE"
MOCK2_PORT=8098
LOG_MOCK2=/tmp/uwuwu-stand-mock2.log
LOG_SRV_MULTI=/tmp/uwuwu-stand-server-multi.log
# мок-1 (дефолтный executor): agent-режим — ведёт uwuwu-цикл до Review
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 MOCK_VERDICTS="result:approve" \
  ./target/debug/mock_opencode >"$LOG_MOCK" 2>&1 &
sleep 1
# мок-2 (named executor «mock2»): dead-режим — спавн «успешен» (create/
# command/prompt живут), но сессия сразу 404 на чтение: красный исполнитель
# идёт по существующей семантике dead-respawn-петли (луп-гвард → Needs
# attention), без тишины
MOCK_MODE=dead MOCK_PORT=$MOCK2_PORT \
  ./target/debug/mock_opencode >"$LOG_MOCK2" 2>&1 &
MOCK2_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$PWD/ops/config.memory-multi.toml UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:$SEED_N:Design;Задачи 1xgames-mock:2047:Design" \
  RUST_LOG=info ./target/debug/uwuwu-server >"$LOG_SRV_MULTI" 2>&1 &
sleep 25   # 2+ тика: оба проекта спавнятся каждый на своём исполнителе
sessions_of() { curl -s --max-time 5 "http://127.0.0.1:$1/api/session" 2>/dev/null | \
  python3 -c "
import json,sys
d = json.load(sys.stdin)
rows = d if isinstance(d, list) else d.get('data', [])
print(' '.join(str(s.get('title') or s.get('id','')) for s in rows))" 2>/dev/null || true; }
M1="$(sessions_of $PORT_MOCK)"; M2="$(sessions_of $MOCK2_PORT)"
echo "$M1" | grep -q "#$SEED_N" || fail "мок-1 не получил сессию uwuwu (дефолт-маршрут сломан): [$M1]"
echo "$M2" | grep -q "#2047" || fail "мок-2 не получил сессию 1xgames-mock (маршрутизация не работает): [$M2]"
echo "$M1" | grep -q "#2047" && fail "кросс-маршрут: 1xgames-mock ушел на дефолтный исполнитель"
echo "$M2" | grep -q "#$SEED_N" && fail "кросс-маршрут: uwuwu ушел на mock2"
say "маршрутизация ок: uwuwu → мок-1, 1xgames-mock → мок-2 (без кроссов)"

task_status_num() { mcp task_get "{\"number\":$1,\"project\":\"$2\"}" | python3 -c "
import json,sys
t = json.load(sys.stdin)['result']['content'][0]['text']
print(t.split(chr(10))[0].split('— ')[-1])" 2>/dev/null; }
sleep 40   # dead-детект + респавны мёртвой сессии 2047 → луп-гвард
grep -q 'loop_escalated' "$LOG_SRV_MULTI" || fail "нет loop_escalated (семантика респавн-петли красного исполнителя сломана)"
S2047="$(task_status_num 2047 1xgames-mock)"
case "$S2047" in
  *attention*|*Needs*) say "красный executor: 2047 → Needs attention (луп-гвард сработал)" ;;
  *) fail "2047 не эскалировал в Needs attention, статус: [$S2047]" ;;
esac

# kill мок-2 (транспортная смерть): strict-union CUP → фолбэк на статусный
# подсчёт, best-effort дример → warn, тик конвейера продолжает жить
BEFORE_TICKS=$(grep -c '"event":"tick.completed"' "$LOG_SRV_MULTI" || true)
kill "$MOCK2_PID" 2>/dev/null || true
sleep 25   # 2+ тика на отказе
AFTER_TICKS=$(grep -c '"event":"tick.completed"' "$LOG_SRV_MULTI" || true)
[ "$AFTER_TICKS" -gt "$BEFORE_TICKS" ] || fail "тик умер после отказа исполнителя (тик-луп встал)"
grep -q 'cup.list_sessions_failed' "$LOG_SRV_MULTI" || fail "нет CUP-фолбэка (strict union не отработал)"
grep -q 'router.list_partial' "$LOG_SRV_MULTI" || fail "нет warn union-скана дримера (best-effort не отработал)"
curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session" >/dev/null || fail "мок-1 недоступен после отказа мок-2"
say "отказ исполнителя: CUP-фолбэк + warn дримера, тик живёт ($BEFORE_TICKS → $AFTER_TICKS тиков)"
pkill -f 'target/debug/uwuwu-server' 2>/dev/null || true
pkill -f 'target/debug/mock_opencode' 2>/dev/null || true
sleep 1
say "фаза 3 ок: маршрутизация по проектам, отказ исполнителя без паралича"

say "ИТОГ: E2E зелёный (observe-чистота + полный цикл с ре-энтри + мульти-экзекьютор)"
