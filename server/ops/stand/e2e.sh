#!/usr/bin/env bash
# e2e.sh — автоматический E2E мок-стенда uwuwu-server (без GitHub/LLM).
#
# Покрывает инцидент #1046 (15.11) комплексно:
#   фаза 1 (observe-яд): строка In Progress с пустым слотом живёт в observe
#     2+ тика — счётчики спавн-петель НЕ заряжаются (state.json чист);
#   фаза 2 (live-цикл с ре-энтри): полный цикл Design → … → Review,
#     где ревью результата один раз даёт request_changes (In Progress →
#     повторный dev-круг → …) — луп-гвард не эскалирует, цикл замыкается;
#   фаза 3 (мульти-экзекьютор): маршрутизация проект → executor, отказ
#     исполнителя без паралича тика (#1203);
#   фаза 4 (гард #1163): живая сессия в env строки при пустой ячейке —
#     спавн держится вторым источником (spawn.held_live_session), после
#     удаления сироты спавн происходит;
#   фаза 5 (self-эхо #1204): медленный мок (акт 11с при тике 10с) —
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
# Стендовые бинари — вне target/debug: чужие прогоны пикают стеки по
# подстроке "target/debug/…" (мультиагентная машина, коллизии 17.11)
mkdir -p target/stand
cp target/debug/uwuwu-server target/debug/mock_opencode target/stand/
# пред-чистка стека: утечка сервера прошлого прогона (убитая сессия не
# отвела trap) занимает 8090 — новый сервер молча exit(1) по bind_failed,
# а цикл ведёт чужой процесс (инцидент прогона тестировщика 17.11)
pkill -f "$PWD/target/stand/uwuwu-server" 2>/dev/null || true
pkill -f "$PWD/target/stand/mock_opencode" 2>/dev/null || true
sleep 1
rm -f "$STATE"

say "фаза 1: observe не заряжает счётчики петель"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 \
  MOCK_VERDICTS="result:request_changes+die" \
  target/stand/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  target/stand/uwuwu-server >"$LOG_SRV_OBS" 2>&1 &
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
  RUST_LOG=info target/stand/uwuwu-server >"$LOG_SRV_LIVE" 2>&1 &
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
DEV_SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N',"slot":"Dev"' "$LOG_SRV_LIVE" | wc -l || true)
[ "$DEV_SPAWNS" -ge 2 ] || fail "ре-энтри не случился (dev-спавнов: $DEV_SPAWNS)"
say "фаза 2 ок: ре-энтри круг пройден (dev-спавнов: $DEV_SPAWNS), эскалаций нет"

say "фаза 3: мульти-экзекьютор — маршрутизация проект → executor + отказ исполнителя (#1203)"
# фаза 2 оставила свой стек (сервер 8090 + мок 8099) — чистим перед стендом
pkill -f "$PWD/target/stand/uwuwu-server" 2>/dev/null || true
pkill -f "$PWD/target/stand/mock_opencode" 2>/dev/null || true
sleep 1
rm -f "$STATE"
MOCK2_PORT="${PORT_MOCK2:-8098}"
LOG_MOCK2="$RUN_DIR/mock2.log"
LOG_SRV_MULTI="$RUN_DIR/server-multi.log"
# stand-копия multi-конфига: именованные executor-ы в TOML не читают env —
# url обоих моков подставляются под фактические порты изоляции
CONF_MULTI="$RUN_DIR/config.memory-multi.toml"
sed -e "s|127.0.0.1:8098|127.0.0.1:$MOCK2_PORT|" \
    -e "s|127.0.0.1:8099|127.0.0.1:$PORT_MOCK|" \
    ops/config.memory-multi.toml > "$CONF_MULTI"
# мок-1 (дефолтный executor): agent-режим — ведёт uwuwu-цикл до Review
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 MOCK_VERDICTS="result:approve" \
  target/stand/mock_opencode >"$LOG_MOCK" 2>&1 &
sleep 1
# мок-2 (named executor «mock2»): dead-режим — спавн «успешен» (create/
# command/prompt живут), но сессия сразу 404 на чтение: красный исполнитель
# идёт по существующей семантике dead-respawn-петли (луп-гвард → Needs
# attention), без тишины
MOCK_MODE=dead MOCK_PORT=$MOCK2_PORT MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" \
  target/stand/mock_opencode >"$LOG_MOCK2" 2>&1 &
MOCK2_PID=$!
sleep 1
UWUWU_SERVER_CONFIG="$CONF_MULTI" UWUWU_SERVER_STATE="$STATE" PORT=$PORT_SRV \
  UWUWU_EXECUTOR_URL="http://127.0.0.1:$PORT_MOCK" UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:$SEED_N:Design;Задачи 1xgames-mock:2047:Design" \
  RUST_LOG=info target/stand/uwuwu-server >"$LOG_SRV_MULTI" 2>&1 &
sleep 25   # 2+ тика: оба проекта спавнятся каждый на своём исполнителе
# диагностика маршрутизации — по журналам моков (list endpoint честно
# фильтрует мёртвые: #1163, dead-сессии mock-2 в list не попадают;
# grep по номеру строки — tracing рендерит поля с ANSI-кодами)
M1="$(grep -c "#$SEED_N" "$LOG_MOCK" 2>/dev/null || true)"; M2="$(grep -c "#2047" "$LOG_MOCK2" 2>/dev/null || true)"
[ "$M1" -ge 1 ] || fail "мок-1 не получил сессию uwuwu (дефолт-маршрут сломан): журналов #$SEED_N нет"
[ "$M2" -ge 1 ] || fail "мок-2 не получил сессию 1xgames-mock (маршрутизация не работает): журналов #2047 нет"
grep -q "#2047" "$LOG_MOCK" && fail "кросс-маршрут: 1xgames-mock ушел на дефолтный исполнитель"
grep -q "#$SEED_N" "$LOG_MOCK2" && fail "кросс-маршрут: uwuwu ушел на mock2"
say "маршрутизация ок: uwuwu → мок-1, 1xgames-mock → мок-2 (без кроссов)"

# Инвариант lifecycle (#1248): create + начальный prompt — ОДИН исполнитель.
# Промпт в чужую сессию — HTTP 404 → action.failed в логе сервера (мок
# отвечает 404 до лога mock.prompt); каждая work-сессия получает свой
# начальный промпт на своём моке. Класс «lifecycle-вызов мимо роутера»
# закрыт стендом, а не разовым фиксом.
if grep -qa 'action.failed' "$LOG_SRV_MULTI"; then
  fail "action.failed при живых исполнителях (lifecycle мимо роутера?): $(grep -am1 'action.failed' "$LOG_SRV_MULTI")"
fi
python3 - "$LOG_MOCK" "$LOG_MOCK2" <<'PY' || fail "lifecycle-инвариант: create+начальный prompt на разных исполнителях"
import json, sys

def events(path):
    out = []
    for line in open(path, errors="ignore"):
        try:
            f = json.loads(line).get("fields")
        except Exception:
            continue
        if isinstance(f, dict):
            out.append(f)
    return out

def check(evs, num, label):
    created = [e for e in evs
               if e.get("event") == "mock.session_created"
               and f"#{num}" in str(e.get("title", ""))
               and "bootstrap" not in str(e.get("title", ""))]
    assert created, f"{label}: нет work-сессий #{num} (спавн не дошёл?)"
    prompted = {e.get("id") for e in evs if e.get("event") == "mock.prompt"}
    missing = sorted({e.get("id") for e in created if e.get("id") not in prompted})
    assert not missing, (
        f"{label}: work-сессии #{num} без начального промпта "
        f"на своём исполнителе: {missing}"
    )

check(events(sys.argv[1]), 2046, "mock1")
check(events(sys.argv[2]), 2047, "mock2")
print("[e2e] lifecycle-инвариант: create + начальный prompt на одном исполнителе (обе доски)")
PY
say "lifecycle-инвариант ок: create + начальный prompt — один исполнитель"

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
pkill -f "$PWD/target/stand/uwuwu-server" 2>/dev/null || true
pkill -f "$PWD/target/stand/mock_opencode" 2>/dev/null || true
sleep 1
say "фаза 3 ок: маршрутизация по проектам, отказ исполнителя без паралича"

say "фаза 4: гард «одна живая сессия на строку» — сирота в env держит спавн (#1163)"
# стек фазы 3 вычищен её хвостом; свой state с нуля
rm -f "$STATE"
# pre-create живой сеанс в env строки (сирота спавна/клейм): спавн держится
# вторым источником — list_sessions матчит директорию <env_root>/<row>.
# MOCK_PAGE_LIMIT=1: сервер итерирует пагинацию list (ревью Common-1) —
# гард обязан видеть сироту и в многостраничном списке
ENV_DIR="/home/yurvon/agents-env/$SEED_N"
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 MOCK_PAGE_LIMIT=1 \
  target/stand/mock_opencode >"$LOG_MOCK" 2>&1 &
sleep 1
ORPHAN_RESP=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session" \
  -H 'Content-Type: application/json' \
  -d "{\"title\":\"orphan-1163\",\"location\":{\"directory\":\"$ENV_DIR\"}}")
ORPHAN_ID=$(echo "$ORPHAN_RESP" | python3 -c "
import json,sys
d = json.load(sys.stdin)
row = d.get('data') or d
print(row.get('id',''))")
[ -n "$ORPHAN_ID" ] || fail "мок не создал сироту-сессию: [$ORPHAN_RESP]"
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  target/stand/uwuwu-server >"$LOG_SRV_LIVE" 2>&1 &
sleep 25   # 2+ тика: спавн держится гардом
SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LIVE" | wc -l || true)
[ "$SPAWNS" -eq 0 ] || fail "спавн случился поверх живой сессии env (гард молчит): $SPAWNS"
grep -q '"event":"spawn.held_live_session"' "$LOG_SRV_LIVE" || fail "нет spawn.held_live_session (гард не сработал)"
say "гард держит: сирота в env, спавнов 0"
# релиз: удаляем сироту — спавн происходит
curl -s --max-time 5 -X DELETE "http://127.0.0.1:$PORT_MOCK/api/session/$ORPHAN_ID" >/dev/null
sleep 25   # тик после релиза
grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LIVE" || fail "спавна нет после удаления сироты (гард перегораживает)"
pkill -f "$PWD/target/stand/uwuwu-server" 2>/dev/null || true
pkill -f "$PWD/target/stand/mock_opencode" 2>/dev/null || true
sleep 1
say "фаза 4 ок: гард держит спавн при живой сессии env и отпускает после её смерти"

# после глобальных чисток фаз #1203/#1163 трекинг pid-ов протух — сброс:
# фаза 5 стартует свои процессы с чистого листа, trap добьёт живые
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"

say "фаза 5: self-эхо — свой MCP-коммент не будит держащую сессию (#1204)"
# Гасим СВОИ остатки (pid-scoped: параллельные стенды соседних сессий
# не трогаем) и поднимаем медленный мок (акт 11с при тике 10с) — каждый
# свежий сеанс на первом wake-тике видит хвост из коммента предшественника
# (создан раньше его активности): own-тест срезает тихо; до фикса тот же
# тик доставлял эхо (лишний акт).
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=11000 \
  target/stand/mock_opencode >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" \
  RUST_LOG=info target/stand/uwuwu-server >"$LOG_SRV_ECHO" 2>&1 &
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
say "фаза 5 ок: sliced=$SLICED, delivered=0 — свои комменты не будят"

say "ИТОГ: E2E зелёный (observe-чистота + полный цикл с ре-энтри + мульти-экзекьютор + гард #1163 + self-эхо); логи: $RUN_DIR"
