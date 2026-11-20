#!/usr/bin/env bash
# e2e.sh — автоматический E2E мок-стенда uwuwu-server (без GitHub/LLM).
#
# Покрывает инцидент #1046 (15.11) комплексно:
#   фаза 1 (observe-яд): строка In Progress с пустым слотом живёт в observe
#     2+ тика — счётчики спавн-петель НЕ заряжаются (state.json чист);
#   фаза 2 (live-цикл с ре-энтри + семейство-непрерывность #1237):
#     полный цикл Design → … → Review, где ревью результата один раз даёт
#     request_changes (+die → respawn) — луп-гвард не эскалирует; дев-сессия
#     переезжает из Design в In Progress (семейная ячейка «Сессия dev»),
#     чужой вердикт будит продолжающуюся сессию;
#   фаза 3 (мульти-экзекьютор): маршрутизация проект → executor, отказ
#     исполнителя без паралича тика (#1203);
#   фаза 4 (гард #1163): живая сессия в env строки при пустой ячейке —
#     спавн держится вторым источником (spawn.held_live_session), после
#     удаления сироты спавн происходит;
#   фаза 5 (self-эхо #1204 в прод-геометрии #1237): медленный мок
#     (акт 11с при тике 10с) — собственный хвост срезается живьём,
#     единственная доставка — чужой вердикт продолжающейся сессии.
#   фаза 6 (слепой hold #1263): исполнитель недоступен — спавн держится
#     (spawn.held_blind_scan, спавнов 0), после возврата исполнителя
#     спавн происходит.
#   фаза 7 (gate-заглушка #1160), фаза 8 (преемпшен CUP #1259),
#   фаза 9 (клин холодного ящика #1293): observe логирует кандидатов,
#     live архивирует; kill-switch без sweep_days.
#   фаза 10 (дример #1306): revival отдрименной сессии → wakeup ТОЙ ЖЕ
#     дример-сессии (ADR-005 1:1), без revival — тишина; watermark+линк
#     в state.json.
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
# Рандир логов/state: /tmp по умолчанию; UWUWU_STAND_RUN_DIR — оверрайд
# (параллельные прогоны и внешние чистки /tmp — изоляция в scratch канона)
RUN_DIR="${UWUWU_STAND_RUN_DIR:-$(mktemp -d /tmp/uwuwu-stand-XXXXXX)}"
mkdir -p "$RUN_DIR"
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
MOCK_PID=""; SRV_PID=""; MOCK2_PID=""

say()  { printf '\033[1;34m[e2e]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[e2e FAIL]\033[0m %s\n' "$*"; KEEP=1; exit 1; }

cleanup() { # только СВОИ процессы (pid-scoped): параллельные стенды не трогаем
  for pid in "$SRV_PID" "$MOCK_PID" "$MOCK2_PID"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  # пер-ран бинари тяжёлые (~2.2 ГБ debug-сборка) — снимаются ВСЕГДА:
  # /tmp — tmpfs, неприбранный рандир добивал бы диск каскадом на чужие
  # прогоны. Рандир с логами сохраняется ТОЛЬКО при fail()-выходе
  # (KEEP=1 — диагностика падения); смерть по set -e минуя fail() (например,
  # отказ команды вне fail-обёртки) сносит рандир целиком вместе с логами
  [ -n "${BIN_SRV:-}" ] && rm -f "$BIN_SRV" "$BIN_MOCK"
  [ "${KEEP:-0}" = "1" ] || rm -rf "$RUN_DIR"
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
# Бинари стенда — per-run копия в рандир: чтение target/debug безопасно при
# чужом прогоне, запись в свежий файл RUN_DIR не даёт ETXTBSY на cp — два
# прогона одного ворктри не мешают друг другу. Утечка убитого стека
# (нетрапленный KILL) держит порт — прогон умрёт громко на bind_failed
# (event http.bind_failed в логе); порты — через env, рандир свой
BIN_SRV="$RUN_DIR/uwuwu-server"
BIN_MOCK="$RUN_DIR/mock_opencode"
cp target/debug/uwuwu-server "$BIN_SRV"
cp target/debug/mock_opencode "$BIN_MOCK"
# утечка нетрапленного KILL держит порт — прогон умрёт ГРОМКО на
# bind_failed (event http.bind_failed), не поведя цикл чужим процессом
rm -f "$STATE"

say "фаза 1: observe не заряжает счётчики петель"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 \
  MOCK_VERDICTS="result:request_changes+die" \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_OBS" 2>&1 &
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
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_LIVE" 2>&1 &
SRV_PID=$!
# сценарий вердиктов: первое ревью результата — на доработку (с мёртвой
# dev-ячейкой), дальше approve
SEEN_OK=0
while [ $SEEN_OK -lt 60 ]; do   # до 5 минут
  SEEN_OK=$((SEEN_OK + 1))
  sleep 5
  S="$(task_status || true)"
  case "$S" in
    *Review) say "цикл замкнут: $S"; break ;;
    *attention*|*Needs*) fail "строка в $S (луп-гвард эскалировал?)" ;;
  esac
done
[ "${S:-}" = "" ] && fail "seed-строка не читается (сервер жив? лог: $LOG_SRV_LIVE)"
case "$S" in *Review) ;; *) fail "цикл не замкнут за 5 мин, статус: $S";; esac

grep -q "loop_escalated" "$LOG_SRV_LIVE" && fail "loop_escalated в live-логе"
# семейная непрерывность (#1237): Design → In Progress НЕ спавнит нового
# дева — design-сессия переезжает (семейная ячейка «Сессия dev»), чужой
# вердикт план-ревью будит продолжающуюся сессию. Единственный slot-Dev
# спавн — respawn после +die (мёртвая семейная ячейка); второй означал бы
# дубль-спавн на переходе — регресс к по-слотовому хранению.
DEV_SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N',"slot":"Dev"' "$LOG_SRV_LIVE" | wc -l || true)
DESIGN_SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N',"slot":"Design"' "$LOG_SRV_LIVE" | wc -l || true)
[ "$DEV_SPAWNS" -eq 1 ] || fail "семейство-непрерывность сломана (дев-спавнов: $DEV_SPAWNS, ожидается 1 — respawn после +die)"
[ "$DESIGN_SPAWNS" -eq 1 ] || fail "design-спавнов: $DESIGN_SPAWNS (ожидается 1)"
DELIVERED=$(grep -c '"event":"wakeup.delivered"' "$LOG_SRV_LIVE" || true)
[ "$DELIVERED" -ge 1 ] || fail "чужой вердикт не разбудил продолжающуюся сессию (delivered: $DELIVERED)"
say "фаза 2 ок: ре-энтри + семейство-непрерывность (design: $DESIGN_SPAWNS, dev-respawn: $DEV_SPAWNS, wake: $DELIVERED), эскалаций нет"

say "фаза 3: мульти-экзекьютор — маршрутизация проект → executor + отказ исполнителя (#1203)"
# фаза 2 оставила свой стек — чистим ЯВНО по pid (чужие прогоны не трогаем)
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""; MOCK2_PID=""
sleep 1
rm -f "$STATE"
MOCK2_PORT=$((PORT_MOCK + 1))   # сессионная изоляция: константа 8098
                                # сталкивала параллельные стенды на фазе 3
LOG_MOCK2="$RUN_DIR/mock2.log"
LOG_SRV_MULTI="$RUN_DIR/server-multi.log"
CONFIG_MULTI="$RUN_DIR/config.memory-multi.toml"
# портируемый конфиг: статические порты 8099/8098 → прогонные
sed -e "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|" \
    -e "s|http://127.0.0.1:8098|http://127.0.0.1:$MOCK2_PORT|" \
    ops/config.memory-multi.toml > "$CONFIG_MULTI"
# мок-1 (дефолтный executor): agent-режим — ведёт uwuwu-цикл до Review
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 MOCK_VERDICTS="result:approve" \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
# мок-2 (named executor «mock2»): dead-режим — спавн «успешен» (create/
# command/prompt живут), но сессия сразу 404 на чтение: красный исполнитель
# идёт по существующей семантике dead-respawn-петли (луп-гвард → Needs
# attention), без тишины
MOCK_MODE=dead MOCK_PORT=$MOCK2_PORT MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" \
  "$BIN_MOCK" >"$LOG_MOCK2" 2>&1 &
MOCK2_PID=$!
sleep 1
# конфиг фазы 3 — в ран-дир (state.json пишется рядом с конфигом; конфиг
# в ops/ делал state общим между прогонами — луп-гварды персистили и
# эскалировали свежие строки мгновенно, #1231 конфликт-kickoff)
CONFIG_MULTI="$RUN_DIR/config-memory-multi.toml"
sed "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|; s|http://127.0.0.1:8098|http://127.0.0.1:$MOCK2_PORT|" \
  ops/config.memory-multi.toml > "$CONFIG_MULTI"
UWUWU_SERVER_CONFIG=$CONFIG_MULTI UWUWU_SERVER_MODE=live PORT=$PORT_SRV \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:$SEED_N:Design;Задачи 1xgames-mock:2047:Design" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_MULTI" 2>&1 &
SRV_PID=$!
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
# dead-детект + респавны мёртвой сессии 2047 → луп-гвард: поллинг вместо
# маргинального фиксированного окна (петле нужно ~6 тиков; дрейф тиков и
# параллельные стенды съедали фиксированные 40с)
ESCALATED=0
for _ in $(seq 1 18); do
  sleep 5
  if grep -q 'loop_escalated' "$LOG_SRV_MULTI" 2>/dev/null; then ESCALATED=1; break; fi
done
[ "$ESCALATED" -eq 1 ] || fail "нет loop_escalated за 90с (семантика респавн-петли красного исполнителя сломана)"
S2047="$(task_status_num 2047 1xgames-mock)"
case "$S2047" in
  *attention*|*Needs*) say "красный executor: 2047 → Needs attention (луп-гвард сработал)" ;;
  *) fail "2047 не эскалировал в Needs attention, статус: [$S2047]" ;;
esac

# kill мок-2 (транспортная смерть): strict-union CUP → фолбэк на статусный
# подсчёт; дример/аналитика на strict — sidecar.tick_failed (решения по
# агрегату честно останавливаются), тик конвейера продолжает жить
BEFORE_TICKS=$(grep -c '"event":"tick.completed"' "$LOG_SRV_MULTI" || true)
kill "$MOCK2_PID" 2>/dev/null || true
sleep 25   # 2+ тика на отказе
AFTER_TICKS=$(grep -c '"event":"tick.completed"' "$LOG_SRV_MULTI" || true)
[ "$AFTER_TICKS" -gt "$BEFORE_TICKS" ] || fail "тик умер после отказа исполнителя (тик-луп встал)"
grep -q 'cup.list_sessions_failed' "$LOG_SRV_MULTI" || fail "нет CUP-фолбэка (strict union не отработал)"
grep -q '"sidecar":"dreamer"' "$LOG_SRV_MULTI" || fail "нет sidecar.tick_failed дримера (strict union не отработал)"
curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session" >/dev/null || fail "мок-1 недоступен после отказа мок-2"
say "отказ исполнителя: CUP-фолбэк + strict-остановка дримера, тик живёт ($BEFORE_TICKS → $AFTER_TICKS тиков)"
kill "$SRV_PID" "$MOCK_PID" "$MOCK2_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" "$MOCK2_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""; MOCK2_PID=""
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
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
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
# конфиг фазы 4 — в ран-дир: state пишется config-adjacent; конфиг из ops/
# делал state общим между прогонами (server/ops/state.json ворктри,
# ревью #1231 раунд 4 Common)
CONFIG_P4="$RUN_DIR/config-memory-p4.toml"
sed "s|http://127.0.0.1:8099|http://127.0.0.1:$PORT_MOCK|; s|http://127.0.0.1:8090|http://127.0.0.1:$PORT_SRV|" \
  ops/config.memory.toml > "$CONFIG_P4"
UWUWU_SERVER_CONFIG=$CONFIG_P4 PORT=$PORT_SRV UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_LIVE" 2>&1 &
SRV_PID=$!
sleep 25   # 2+ тика: спавн держится гардом
SPAWNS=$(grep -oE '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LIVE" | wc -l || true)
[ "$SPAWNS" -eq 0 ] || fail "спавн случился поверх живой сессии env (гард молчит): $SPAWNS"
grep -q '"event":"spawn.held_live_session"' "$LOG_SRV_LIVE" || fail "нет spawn.held_live_session (гард не сработал)"
say "гард держит: сирота в env, спавнов 0"
# релиз: удаляем сироту — спавн происходит
curl -s --max-time 5 -X DELETE "http://127.0.0.1:$PORT_MOCK/api/session/$ORPHAN_ID" >/dev/null
sleep 25   # тик после релиза
grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LIVE" || fail "спавна нет после удаления сироты (гард перегораживает)"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 4 ок: гард держит спавн при живой сессии env и отпускает после её смерти"

say "фаза 5: self-эхо в прод-геометрии — свой хвост срезается, чужой вердикт будит держащую сессию (#1204, #1237)"
# Гасим СВОИ остатки (pid-scoped: параллельные стенды соседних сессий
# не трогаем) и поднимаем медленный мок (акт 11с при тике 10с) — тайминг
# own-теста: свежий сеанс на первом wake-тике видит хвост из коммента
# предшественника (создан раньше его активности) — срезается тихо.
# Прод-геометрия (#1237): дев-сессия переезжает из Design в In Progress
# (семейная ячейка «Сессия dev»), wake-путь получает адресата.
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=11000 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Design" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_ECHO" 2>&1 &
SRV_PID=$!
# Цикл Design → … → Review на дефолтных approve с лагом актов: дев-сессия
# продолжается в In Progress (семейная ячейка), собственные комменты
# срезаются на wake-тиках (wakeup.self_echo_sliced), чужой вердикт
# план-ревью доезжает до продолжающейся сессии (wakeup.delivered).
SEEN_OK=0
while [ $SEEN_OK -lt 72 ]; do
  SEEN_OK=$((SEEN_OK + 1))
  sleep 5
  S="$(task_status || true)"
  case "$S" in
    *Review) say "цикл замкнут: $S"; break ;;
    *attention*|*Needs*) fail "строка в $S" ;;
  esac
done
case "$S" in *Review) ;; *) fail "цикл не замкнут за 6 мин, статус: $S";; esac
sleep 12   # окно эха: тик после idle-фазы последних актов
# Числа пре-зарегистрированы до прогона (#1237): delivered == 1 (вердикт
# план-ревью → продолжающаяся дев-сессия; 0 = per-slot-регресс, >1 = эхо),
# sliced >= 3 (свой план-коммент в первом wake-тике продолжающейся сессии +
# хвосты свежих result/тест-сессий). Иное число — разбор причины, не правка
# ассерта.
DELIVERED=$(grep -c '"event":"wakeup.delivered"' "$LOG_SRV_ECHO" || true)
SLICED=$(grep -c '"event":"wakeup.self_echo_sliced"' "$LOG_SRV_ECHO" || true)
[ "$SLICED" -ge 3 ] || fail "own-срезов: $SLICED (ожидалось >=3 — план-коммент + хвосты свежих result/тест-сессий)"
[ "$DELIVERED" -eq 1 ] || fail "wake-доставок: $DELIVERED (ожидается ровно 1 — вердикт план-ревью продолжающейся сессии; 0 = per-slot-регресс, >1 = эхо)"
say "фаза 5 ок: sliced=$SLICED, delivered=$DELIVERED — свой хвост срезается, чужой вердикт будит держащую сессию"

say "фаза 6: слепой hold — список исполнителя недоступен, спавн без зрения невозможен (#1263)"
# Гасим СВОИ остатки (pid-scoped); мок НЕ поднимаем — union-скан падает
# на каждом тике, инверсия деградации держит спавн до восстановления
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
LOG_SRV_BLIND="$RUN_DIR/server-blind.log"
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_BLIND" 2>&1 &
SRV_PID=$!
sleep 25   # 2+ тика на слепоте: спавн держится
SPAWNS=$(grep -oE '"event":"spawn.done"' "$LOG_SRV_BLIND" | wc -l || true)
[ "$SPAWNS" -eq 0 ] || fail "спавн случился при недоступном исполнителе (слепой спавн): $SPAWNS"
grep -q '"event":"spawn.held_blind_scan"' "$LOG_SRV_BLIND" \
  || fail "нет spawn.held_blind_scan (слепой hold не сработал)"
grep -q '"event":"tick.completed"' "$LOG_SRV_BLIND" || fail "тик умер на слепоте"
say "слепой hold держит: исполнитель недоступен, спавнов 0, тик живёт"
# восстановление: мок возвращаем — спавн происходит
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 25
grep -q '"event":"spawn.done"' "$LOG_SRV_BLIND" \
  || fail "спавна нет после возврата исполнителя (hold перегораживает)"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 6 ок: слепой hold держит спавн при мёртвом исполнителе и отпускает после возврата"

say "фаза 7: gate-заглушка #1160 — исполнитель жив, но гейт отдаёт HTML «Войти» (без-cookie): спавн держится"
# Отличие от фазы 6: HTTP отвечает 200 — до фикса call() молча парсил HTML
# в Null и скан был «успешно пуст» (Ok(пусто) → спавн насквозь). Контракт
# фикса: Err сниффа → strict-скан Err → hold; после снятия заглушки спавн.
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
LOG_SRV_GATE="$RUN_DIR/server-gate.log"
MOCK_MODE=gate MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_GATE" 2>&1 &
SRV_PID=$!
sleep 25   # 2+ тика в окне заглушки: спавн держится
SPAWNS=$(grep -oE '"event":"spawn.done"' "$LOG_SRV_GATE" | wc -l || true)
[ "$SPAWNS" -eq 0 ] || fail "спавн случился в окне заглушки (HTML прошёл как «успешно пусто»): $SPAWNS"
grep -q '"event":"spawn.held_blind_scan"' "$LOG_SRV_GATE" \
  || fail "нет spawn.held_blind_scan на gate-заглушке (скан не дал Err?)"
grep -q '"event":"tick.completed"' "$LOG_SRV_GATE" || fail "тик умер на заглушке"
grep -q 'gate HTML-заглушка' "$LOG_SRV_GATE" \
  || say "примечание: маркер «gate HTML-заглушка» не найден в логе (Err может идти статусным чеком 405 на POST — ок)"
say "gate-заглушка держит: спавнов 0, hold сработал, тик живёт"
# восстановление: заглушку снимаем (мок в plain) — спавн происходит
kill "$MOCK_PID" 2>/dev/null || true
wait "$MOCK_PID" 2>/dev/null || true
MOCK_PID=""
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 25
grep -q '"event":"spawn.done"' "$LOG_SRV_GATE" \
  || fail "спавна нет после снятия заглушки (hold перегораживает)"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 7 ок: окно gate-заглушки держит спавн и отпускает после снятия (#1160)"

say "фаза 8: преемпшен CUP — понижение приоритета busy-актива отдаёт слот (#1259)"
# свой стек (фаза 5 вычищена своим хвостом) и свой конфиг с капом 1:
# один слот — жертва держит, кандидат ждёт; мок busy — жертва не отвечает
CONFIG_PREEMPT="$RUN_DIR/config.preempt.toml"
cat > "$CONFIG_PREEMPT" <<EOF
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
cup = 1
peak = { enabled = false, start_min = 0, end_min = 0 }

[[project]]
name = "uwuwu"
board = "Задачи uwuwu"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"
EOF
rm -f "$STATE"
LOG_SRV_PRE="$RUN_DIR/server-preempt.log"
MOCK_MODE=busy MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG_PREEMPT UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="2046:In Progress:High;2048:In Progress:High;2049:In Progress:High;2050:In Progress:High" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_PRE" 2>&1 &
SRV_PID=$!
# тики до HeldCup: жертва 2046 спавнится и busy (не отвечает), кандидат
# 2048 held — high-жертва не гасится (can_preempt false) → cup.preempt.no_victim
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1))
  sleep 5
  grep -q '"event":"cup.preempt.no_victim"' "$LOG_SRV_PRE" && break
done
grep -q '"event":"cup.preempt.no_victim"' "$LOG_SRV_PRE" \
  || fail "кандидат не достиг HeldCup (нет no_victim за минуту)"
SPAWN2046=$(grep -oE '"event":"spawn.done","number":2046,"slot":"Dev"' "$LOG_SRV_PRE" | wc -l || true)
[ "$SPAWN2046" -ge 1 ] || fail "жертва 2046 не заспавнена"
say "held-фаза ок: high-жертва держит слот (no_victim), спавнов кандидата 0"
# владелец понижает приоритет busy-актива: high → low
mcp task_edit '{"number":2046,"project":"uwuwu","priority":"low"}' | grep -q '"isError":false' \
  || fail "task_edit priority не применился (мок-hub не носит приоритет?)"
# ближайший тик: cup.preempt (abort+delete жертвы) → слот кандидату
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1))
  sleep 5
  grep -q '"event":"cup.preempt"' "$LOG_SRV_PRE" && break
done
grep -q '"event":"cup.preempt","waiter":2048,"victim":2046' "$LOG_SRV_PRE" \
  || fail "cup.preempt не случился (waiter 2048 / victim 2046)"
grep -q '"event":"cup.preempt.cleared_cell"' "$LOG_SRV_PRE" \
  || fail "гашение не чистит ячейку жертвы (cup.preempt.cleared_cell отсутствует)"
grep -q '"event":"spawn.done","number":2048,"slot":"Dev"' "$LOG_SRV_PRE" \
  || fail "кандидат 2048 не получил слот после преемпшена"
grep -q 'mock.session_aborted' "$LOG_MOCK" \
  || fail "abort-рута исполнителя не дёрнулась"
LEFT=$(curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session" | grep -c '"title":"#2046 ' || true)
[ "$LEFT" -eq 0 ] || fail "жертва 2046 не удалена с исполнителя (delete пропущен?)"
grep -q '"preempts":1,' "$LOG_SRV_PRE" || fail "tick.completed не несёт preempts=1"
grep -q 'loop_escalated' "$LOG_SRV_PRE" && fail "луп-гвард эскалировал"
say "#1412: гашение — сброс ячейки и луп-гварда (cup.preempt.cleared_cell), 2048 заспавнена; ×5-цикл — unit-тест cup_preempt_wave"
# Волна 2 (#1412): 2049 High гасит 2048 (can_preempt High/Low). Гвард 2048
# к этому моменту = 1 (один спавн), поэтому эскалации нет; сброс гварда
# 2046 произошёл в первой волне. ×5-контракт — unit-тест cup_preempt_wave.
mcp task_edit '{"number":2048,"project":"uwuwu","priority":"low"}' | grep -q '"isError":false' \
  || fail "task_edit priority 2048 не применился"
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  [ "$(grep -c '"event":"cup.preempt"' "$LOG_SRV_PRE" || true)" -ge 2 ] && break
done
[ "$(grep -c '"event":"cup.preempt"' "$LOG_SRV_PRE" || true)" -ge 2 ] \
  || fail "вторая волна: 2049 не гасит 2048 (второго cup.preempt нет)"
grep -q '"event":"cup.preempt","waiter":2049,"victim":2048' "$LOG_SRV_PRE" \
  || fail "вторая волна: waiter/victim не 2049/2048"
grep -q '"event":"spawn.done","number":2049,"slot":"Dev"' "$LOG_SRV_PRE" \
  || fail "кандидат 2049 не получил слот после второй волны"
grep -q 'loop_escalated' "$LOG_SRV_PRE" && fail "луп-гвард эскалировал на второй волне (фикс сломан)"
say "#1412: вторая волна — 2049 гасит 2048, эскалаций нет"
# Волна 3 (#1412, low #6): 2050 High гасит 2049 (can_preempt High/Low) —
# третий преемпшен за окно → IncidentLoudness достигает порога 3 и шлёт
# cup.preempt_unnotified (notify в e2e не настроен → off-ветка).
mcp task_edit '{"number":2049,"project":"uwuwu","priority":"low"}' | grep -q '"isError":false' \
  || fail "task_edit priority 2049 не применился"
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  [ "$(grep -c '"event":"cup.preempt"' "$LOG_SRV_PRE" || true)" -ge 3 ] && break
done
[ "$(grep -c '"event":"cup.preempt"' "$LOG_SRV_PRE" || true)" -ge 3 ] \
  || fail "третья волна: 2050 не гасит 2049 (третьего cup.preempt нет)"
grep -q '"event":"cup.preempt","waiter":2050,"victim":2049' "$LOG_SRV_PRE" \
  || fail "третья волна: waiter/victim не 2050/2049"
grep -q '"event":"spawn.done","number":2050,"slot":"Dev"' "$LOG_SRV_PRE" \
  || fail "кандидат 2050 не получил слот после третьей волны"
grep -q '"event":"cup.preempt_unnotified"' "$LOG_SRV_PRE" \
  || fail "IncidentLoudness не достиг порога 3 (cup.preempt_unnotified отсутствует)"
grep -q 'loop_escalated' "$LOG_SRV_PRE" && fail "луп-гвард эскалировал на третьей волне (фикс сломан)"
say "#1412: третья волна — 2050 гасит 2049, IncidentLoudness → cup.preempt_unnotified, эскалаций нет"
# pid-scoped зачистка (конвенция #1263): чужие стенды не трогаем
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
say "фаза 8 ок: понижение приоритета отдало слот (2046 abort+delete, 2048 заспавнен); #1412: волна 2 (2048 гасится 2049), жертва возвращается plain-спавном, эскалаций нет"

say "фаза 9: клин холодного ящика #1293 — observe логирует кандидатов, live архивирует"
CONFIG_SWEEP="$RUN_DIR/config.sweep.toml"
cat > "$CONFIG_SWEEP" <<EOF
mode = "live"
poll_sec = 10
sweep_days = 7
port = $PORT_SRV

[hub]
repo = "yurvon-screamo/uwuwu-hub"
driver = "memory"

[executor]
name = "local-mock"
url = "http://127.0.0.1:$PORT_MOCK"

[policy]
cup = 1
peak = { enabled = false, start_min = 0, end_min = 0 }

[[project]]
name = "uwuwu"
board = "Задачи uwuwu"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"
EOF
# 2049 закрыта 14 суток назад (кандидат), 2050 — сутки назад (свежая);
# done-сиды не занимают слоты CUP — мок-исполнитель не нужен живым ходам,
# но sidecar sweeper дергает только хаб (без исполнителя)
rm -f "$STATE"
LOG_SRV_SWEEP="$RUN_DIR/server-sweep.log"
MOCK_MODE=idle MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=100 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
# 9a: observe — внутренний гейт клина: кандидаты в логе, строка на доске
UWUWU_SERVER_CONFIG=$CONFIG_SWEEP UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:2049:Done:low:14;Задачи uwuwu:2050:Done:low:1" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_SWEEP" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 10 ]; do
  SEEN=$((SEEN + 1))
  sleep 4
  grep -q '"event":"sweep.observe"' "$LOG_SRV_SWEEP" && break
done
grep -q '"event":"sweep.observe"' "$LOG_SRV_SWEEP" \
  || fail "observe-клин не показал кандидатов (внутренний гейт слепой)"
mcp task_list '{"project":"uwuwu"}' | grep -q '2049' \
  || fail "observe-клин мутировал: 2049 исчезла из task_list в observe"
say "9a ок: observe показал кандидата и не мутировал"
# 9b: live — архивация: 2049 уходит из снапшота, 2050 остаётся
kill "$SRV_PID" 2>/dev/null || true
wait "$SRV_PID" 2>/dev/null || true
rm -f "$STATE"
UWUWU_SERVER_CONFIG=$CONFIG_SWEEP UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:2049:Done:low:14;Задачи uwuwu:2050:Done:low:1" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_SWEEP" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 10 ]; do
  SEEN=$((SEEN + 1))
  sleep 4
  grep -q '"event":"sweep.completed"' "$LOG_SRV_SWEEP" && break
done
grep -q '"event":"sweep.completed"' "$LOG_SRV_SWEEP" \
  || fail "live-клин не отчитался проходом (sweep.completed нет за 40с)"
SWEEP_LIST=$(mcp task_list '{"project":"uwuwu"}')
echo "$SWEEP_LIST" | grep -q '2049' \
  && fail "live-клин не сработал: 2049 осталась в task_list"
echo "$SWEEP_LIST" | grep -q '2050' \
  || fail "live-клин переложил свежую: 2050 исчезла из task_list"
grep -q '"event":"sweep.failed"' "$LOG_SRV_SWEEP" \
  && fail "sweep.failed в фазе 9 — архивация мок-хаба не удалась"
# pid-scoped зачистка
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
say "фаза 9 ок: 2049 ушла в архив и из task_list, 2050 свежая осталась"

say "фаза 9c: kill-switch — конфиг БЕЗ sweep_days (serde-дефолт 0): клина нет"
CONFIG_NOSWEEP="$RUN_DIR/config.nosweep.toml"
grep -v '^sweep_days' "$CONFIG_SWEEP" > "$CONFIG_NOSWEEP"
rm -f "$STATE"
LOG_SRV_NOSWEEP="$RUN_DIR/server-nosweep.log"
MOCK_MODE=idle MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=100 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG_NOSWEEP UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="Задачи uwuwu:2051:Done:low:14" \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_NOSWEEP" 2>&1 &
SRV_PID=$!
# 4 тика: sweep_days=0 обязан держать сайдкар выключенным структурно
sleep 45
grep -aq '"event":"sweep.' "$LOG_SRV_NOSWEEP" \
  && fail "kill-switch сломан: sweep-события при sweep_days=0"
NOSWEEP_LIST=$(mcp task_list '{"project":"uwuwu"}')
echo "$NOSWEEP_LIST" | grep -q '2051' \
  || fail "клин без конфига снёс строку 2051 (serde-дефолт 0 = агрессивный клин)"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
say "фаза 9c ок: конфиг без sweep_days — sweeper структурно выключен, 2051 на доске"

say "фаза 10: дример #1306 — revival отдрименной сессии даёт повторный прогон в ту же дример-сессию (ADR-005 1:1)"
# Свой стек: свежий мок (agent — ходы завершаются с outcome+токенами),
# свежий state. Время — стендовой ручкой /age (updated_ms -= ms): сессия
# «уснула» 31+ минут назад без реального ожидания. Работа дримера видна
# по событиям dreamer.spawned/dreamer.wakeup в логе сервера и state.json.
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
LOG_SRV_DREAM="$RUN_DIR/server-dream.log"
MOCK_MODE=agent MOCK_PORT=$PORT_MOCK MOCK_MCP_URL="http://127.0.0.1:$PORT_SRV" \
  MOCK_MCP_TOKEN="$BEARER" MOCK_STEP_MS=300 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_DREAM" 2>&1 &
SRV_PID=$!
# юзер: рабочая сессия отработала (финал, токены > 0)
WORK_RESP=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session" \
  -H 'Content-Type: application/json' -d '{"title":"dream-work-1306"}')
WORK_ID=$(echo "$WORK_RESP" | python3 -c "
import json,sys
d = json.load(sys.stdin)
row = d.get('data') or d
print(row.get('id',''))")
[ -n "$WORK_ID" ] || fail "мок не создал work-сессию: [$WORK_RESP]"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$WORK_ID/prompt" \
  -H 'Content-Type: application/json' -d '{"text":"поработал над задачей"}' >/dev/null
sleep 1   # акт 300мс: outcome succeeded, токены
# сессия уснула 31+ минут назад → дример обязан спавнить носителя
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$WORK_ID/age" \
  -H 'Content-Type: application/json' -d '{"ms":1900000}' >/dev/null
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1))
  sleep 5
  grep -q '"event":"dreamer.spawned"' "$LOG_SRV_DREAM" && break
done
grep -q '"event":"dreamer.spawned"' "$LOG_SRV_DREAM" \
  || fail "дример не спавнил носителя по уснувшей сессии (кандидат не найден?)"
DREAM_ID=$(curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session" | python3 -c "
import json,sys
dreams = [s['id'] for s in json.load(sys.stdin).get('data', []) if s.get('agent') == 'dreamer']
print(dreams[0] if dreams else '')")
[ -n "$DREAM_ID" ] || fail "спавна нет на исполнителе (сессии agent=dreamer нет)"
# сайдкар-мутации (метка/линк) сбрасываются save'ом СЛЕДУЮЩЕГО main-тика
# (pre-existing паттерн, ревью #1306) — ждём флаш в файл, не читаем сразу
STATE_OK=0
for _ in $(seq 1 6); do
  sleep 5
  python3 - "$STATE" "$WORK_ID" "$DREAM_ID" <<'PY' 2>/dev/null && STATE_OK=1 && break
import json, sys
st = json.load(open(sys.argv[1]))
work, dream = sys.argv[2], sys.argv[3]
assert st["dreamer_links"].get(work) == dream, "линк ещё не слился"
assert work in st["dreamer"], "watermark ещё не слился"
PY
done
[ "$STATE_OK" -eq 1 ] || fail "state после первого дрима неверен (watermark/линк не записаны)"
python3 - "$STATE" "$WORK_ID" "$DREAM_ID" <<'PY' || fail "state после первого дрима неверен"
import json, sys
st = json.load(open(sys.argv[1]))
work, dream = sys.argv[2], sys.argv[3]
assert st["dreamer_links"].get(work) == dream, f"линк 1:1 не записан: {st['dreamer_links']}"
assert work in st["dreamer"], f"watermark не записан: {st['dreamer']}"
print("[e2e] первый дрим: носитель заспавнен, watermark+линк в state")
PY
say "первый дрим ок: носитель $DREAM_ID, watermark+линк записаны"
# revival: юзер дописывает в отдрименную сессию → снова уснула → wakeup
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$WORK_ID/prompt" \
  -H 'Content-Type: application/json' -d '{"text":"дополнение после дрима"}' >/dev/null
sleep 1
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$WORK_ID/age" \
  -H 'Content-Type: application/json' -d '{"ms":1900000}' >/dev/null
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1))
  sleep 5
  grep -q '"event":"dreamer.wakeup"' "$LOG_SRV_DREAM" && break
done
grep -q '"event":"dreamer.wakeup"' "$LOG_SRV_DREAM" \
  || fail "revival отдрименной сессии не дал повторного прогона (баг #1306 жив?)"
SPAWNS=$(grep -c '"event":"dreamer.spawned"' "$LOG_SRV_DREAM" || true)
[ "$SPAWNS" -eq 1 ] || fail "revival породил новый спавн (инвариант 1:1 сломан): $SPAWNS"
SESSIONS=$(curl -s --max-time 5 "http://127.0.0.1:$PORT_MOCK/api/session" | python3 -c "
import json,sys
print(len(json.load(sys.stdin).get('data', [])))")
[ "$SESSIONS" -eq 2 ] || fail "на исполнителе лишние сессии (новый носитель?): $SESSIONS"
STATE_OK=0
for _ in $(seq 1 6); do
  sleep 5
  python3 - "$STATE" "$WORK_ID" "$DREAM_ID" <<'PY' 2>/dev/null && STATE_OK=1 && break
import json, sys
st = json.load(open(sys.argv[1]))
work, dream = sys.argv[2], sys.argv[3]
assert st["dreamer_links"].get(work) == dream, "линк ещё не слился/перезаписан"
PY
done
[ "$STATE_OK" -eq 1 ] || fail "state после revival неверен (линк перезаписан?)"
python3 - "$STATE" "$WORK_ID" "$DREAM_ID" <<'PY' || fail "state после revival неверен"
import json, sys
st = json.load(open(sys.argv[1]))
work, dream = sys.argv[2], sys.argv[3]
assert st["dreamer_links"].get(work) == dream, f"линк перезаписан (не 1:1): {st['dreamer_links']}"
print("[e2e] revival: wakeup той же дример-сессии, линк не менялся")
PY
# контроль зелёного фона: без revival — тишина на 2+ тиках
sleep 25
WAKEUPS=$(grep -c '"event":"dreamer.wakeup"' "$LOG_SRV_DREAM" || true)
[ "$WAKEUPS" -eq 1 ] || fail "без revival дример не молчит (wakeup'ов: $WAKEUPS)"
SPAWNS=$(grep -c '"event":"dreamer.spawned"' "$LOG_SRV_DREAM" || true)
[ "$SPAWNS" -eq 1 ] || fail "без revival дример спавнит (спавнов: $SPAWNS)"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 10 ок: revival → wakeup той же сессии (1:1), без revival — тишина"

say "фаза 11: phantom-записи носителя → громкий отказ MCP-мутации (#1331)"
wait_srv() { # /health под bearer до готовности (≤20с)
  for _ in $(seq 1 20); do
    curl -s --max-time 2 -H "Authorization: Bearer $BEARER" \
      "http://127.0.0.1:$PORT_SRV/health" | grep -q . && return 0
    sleep 1
  done
  return 1
}
LOG_SRV_PHANTOM="$RUN_DIR/server-phantom.log"
LOG_SRV_HONEST="$RUN_DIR/server-honest.log"
# ветка фантома: носитель отвечает успехом и ничего не записывает —
# контрольное чтение юзкейса обязано превращать «успех» в громкий отказ
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Analys" UWUWU_MOCK_PHANTOM_WRITES=1 RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_PHANTOM" 2>&1 &
SRV_PID=$!
wait_srv || fail "фантом-сервер не поднялся (health тишина)"
sleep 1
CREATED=$(mcp task_create '{"title":"t: phantom probe","body":"## Постановка\n\nprobe-1331\n","priority":"low","automerge":true}')
PH_N=$(printf '%s' "$CREATED" | python3 -c "
import json,sys
t = json.load(sys.stdin)['result']['content'][0]['text']
print(t.split('#')[1].split(' ')[0])") || fail "create на фантом-стенде не отдал номер: $CREATED"
BODY_RESP=$(mcp task_body "{\"number\":$PH_N,\"body\":\"## Постановка\n\nфантомное тело\n\"}")
printf '%s' "$BODY_RESP" | grep -q '"isError":true' \
  || fail "task_body на фантоме не отказал: $BODY_RESP"
printf '%s' "$BODY_RESP" | grep -q 'не подтвердилась' \
  || fail "текст отказа body без вердикта контрольного чтения: $BODY_RESP"
COMMENT_RESP=$(mcp task_comment "{\"number\":$PH_N,\"text\":\"probe-1331\"}")
printf '%s' "$COMMENT_RESP" | grep -q '"isError":true' \
  || fail "task_comment на фантоме не отказал: $COMMENT_RESP"
printf '%s' "$COMMENT_RESP" | grep -q 'не подтвердился' \
  || fail "текст отказа comment без вердикта контрольного чтения: $COMMENT_RESP"
EDIT_RESP=$(mcp task_edit "{\"number\":$PH_N,\"branch\":\"work/task-1331\"}")
printf '%s' "$EDIT_RESP" | grep -q '"isError":true' \
  || fail "task_edit(branch) на фантоме не отказал: $EDIT_RESP"
printf '%s' "$EDIT_RESP" | grep -q 'не подтвердилась' \
  || fail "текст отказа edit без вердикта контрольного чтения: $EDIT_RESP"
CARRIER=$(mcp task_get "{\"number\":$PH_N}")
printf '%s' "$CARRIER" | grep -q 'фантомное тело' \
  && fail "фантомная запись попала в носитель: $CARRIER"
kill "$SRV_PID" 2>/dev/null || true; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""
sleep 1
say "phantom-ветка ок: body/comment/edit громко отказали, носитель чист"
# ветка честного носителя: тот же юзкейс без флага — успех без маркера
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=observe \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:Analys" RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_HONEST" 2>&1 &
SRV_PID=$!
wait_srv || fail "честный сервер не поднялся (health тишина)"
sleep 1
CREATED=$(mcp task_create '{"title":"t: honest probe","body":"## Постановка\n\nprobe-1331\n","priority":"low","automerge":true}')
H_N=$(printf '%s' "$CREATED" | python3 -c "
import json,sys
t = json.load(sys.stdin)['result']['content'][0]['text']
print(t.split('#')[1].split(' ')[0])") || fail "create на честном стенде не отдал номер: $CREATED"
HONEST=$(mcp task_body "{\"number\":$H_N,\"body\":\"## Постановка\n\nчестное тело\n\"}")
printf '%s' "$HONEST" | grep -q 'заменено' || fail "task_body на честном носителе отказал: $HONEST"
printf '%s' "$HONEST" | grep -q 'не подтверждена' \
  && fail "честная запись получила маркер непроверенности: $HONEST"
HONEST_C=$(mcp task_comment "{\"number\":$H_N,\"text\":\"probe-ok\"}")
printf '%s' "$HONEST_C" | grep -q 'коммент отправлен' \
  || fail "task_comment на честном носителе отказал: $HONEST_C"
kill "$SRV_PID" 2>/dev/null || true; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""
sleep 1
say "фаза 11 ок: фантом → громкий отказ, честный носитель → успех без маркера"

say "фаза 12: lost-сессия #1288 — десинк (доставка 404 при живом списке) → счётчик → Needs attention"
# Стек фазы 11 вычищен её хвостом; свой state. AFTER=1: начальный
# спавн-промпт успешен, вся дальнейшая доставка (пин агента + prompt)
# → 404 при живом list — serve-рассинхрон инцидента #16 («проба Alive,
# доставка отказывает»). Потеря по сталл-пути (age-ручка): инцидент #16
# ретраил именно сталл-пинг (тик-интервал).
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
rm -f "$STATE"
LOG_SRV_LOST="$RUN_DIR/server-lost.log"
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 MOCK_PROMPT_404_AFTER=1 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_LOST" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LOST" && break
done
grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LOST" \
  || fail "спавна dev-сессии нет (стенд не поднялся): лог $LOG_SRV_LOST"
# сессия «замерла» 31+ минут назад → сталл-пинг на каждом тике
# (mark_stall_ping при Err не пишется) — доставка 404 раз за тик
LOST_SESSION=$(grep -oE '"session":"ses_mock[0-9a-f]+"' "$LOG_SRV_LOST" | head -1 | cut -d'"' -f4)
[ -n "$LOST_SESSION" ] || fail "session id спавна не найден в логе"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$LOST_SESSION/age" \
  -H 'Content-Type: application/json' -d '{"ms": 1900000}' >/dev/null
# порог 3 подряд → lost.escalated
SEEN=0
while [ $SEEN -lt 18 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  grep -q '"event":"lost.escalated"' "$LOG_SRV_LOST" && break
done
grep -q '"event":"lost.escalated"' "$LOG_SRV_LOST" \
  || fail "нет lost.escalated за 90с (десинк-парковка не сработала)"
S="$(task_status)"
case "$S" in
  *attention*|*Needs*) say "строка запаркована: $S" ;;
  *) fail "строка не в Needs attention после lost.escalated: $S" ;;
esac
mcp task_get "{\"number\":$SEED_N}" | grep -q 'Сессия потеряна' \
  || fail "нет коммента с причиной парковки на строке"
# анти-дубль: respawn при живом списке запрещён — спавн ровно один
SPAWNS=$(grep -c '"event":"spawn.done"' "$LOG_SRV_LOST" || true)
[ "$SPAWNS" -eq 1 ] || fail "анти-дубль сломан: спавнов $SPAWNS (ожидается 1)"
grep -q '"event":"delivery.session_lost"' "$LOG_SRV_LOST" \
  || fail "нет события delivery.session_lost (классификация не работает)"
# петля остановлена: после парковки новых lost-доставок нет
sleep 21
LOST_TOTAL=$(grep -c '"event":"delivery.session_lost"' "$LOG_SRV_LOST" || true)
[ "$LOST_TOTAL" -le 4 ] || fail "ретрай-петля не остановлена после парковки: lost-доставок $LOST_TOTAL"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 12 ок: доставка 404 при живом списке — счётчик 3, парковка с причиной, respawn запрещён, петля остановлена"

say "фаза 13: lost-resurrection #1288 — сессия скрыта из list, доставка 404 → respawn-петля"
# lost-ручка (POST /api/lost, включается ПОСЛЕ спавна): сессия ячейки
# скрыта из list, GET 200 (probe Alive), prompt/switch_agent → 404.
# lost-рука видит «сессии нет в списке» → resurrection гвардами
# Dead-ветки; 404 свежих спавнов — сирота-делит (анти-ре-энтри),
# луп-гвард >4 паркует («Респавн-петля», юнит-класс T2 живьём).
rm -f "$STATE"
LOG_SRV_LRES="$RUN_DIR/server-lres.log"
MOCK_MODE=plain MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=1500 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG=$CONFIG UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_MODE=live \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$SEED_N:In Progress" RUST_LOG=info \
  "$BIN_SRV" >"$LOG_SRV_LRES" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 12 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LRES" && break
done
grep -q '"event":"spawn.done","number":'$SEED_N "$LOG_SRV_LRES" \
  || fail "спавна нет в lost-resurrection-фазе: лог $LOG_SRV_LRES"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/lost" \
  -H 'Content-Type: application/json' \
  -d "{\"directory\": \"/home/yurvon/agents-env/$SEED_N\"}" >/dev/null
LOST_SESSION=$(grep -oE '"session":"ses_mock[0-9a-f]+"' "$LOG_SRV_LRES" | head -1 | cut -d'"' -f4)
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT_MOCK/api/session/$LOST_SESSION/age" \
  -H 'Content-Type: application/json' -d '{"ms": 1900000}' >/dev/null
SEEN=0
while [ $SEEN -lt 24 ]; do
  SEEN=$((SEEN + 1)); sleep 5
  grep -q '"event":"respawn.loop_escalated"' "$LOG_SRV_LRES" && break
done
grep -q '"event":"respawn.loop_escalated"' "$LOG_SRV_LRES" \
  || fail "нет respawn.loop_escalated за 120с (lost-resurrection не вошла в луп-гвард)"
grep -q '"event":"delivery.session_lost"' "$LOG_SRV_LRES" \
  || fail "нет delivery.session_lost (lost-рука не маршрутизировала?)"
S="$(task_status)"
case "$S" in
  *attention*|*Needs*) say "луп-гвард запарковал: $S" ;;
  *) fail "строка не в Needs attention после respawn-петли: $S" ;;
esac
mcp task_get "{\"number\":$SEED_N}" | grep -q 'Респавн-петля' \
  || fail "нет коммента «Респавн-петля» на строке"
# анти-ре-энтри: одна respawn-попытка за тик — create/delete пары чисты
# (bootstrap+work пары + сироты; deleted = created - живые work-сессии)
CREATED=$(grep -c '"event":"mock.session_created"' "$LOG_MOCK" || true)
DELETED=$(grep -c '"event":"mock.session_deleted"' "$LOG_MOCK" || true)
[ "$DELETED" -ge 4 ] || fail "сироты не делитятся (deleted: $DELETED)"
[ "$CREATED" -eq "$((DELETED + 1))" ] \
  || fail "рассинхрон create/delete: created=$CREATED deleted=$DELETED"
DEADS=$(grep -c '"event":"wakeup.dead_session"' "$LOG_SRV_LRES" || true)
[ "$DEADS" -ge 4 ] || fail "resurrection-попыток меньше порога: $DEADS"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 13 ок: lost-resurrection через lost-руку, луп-гвард парковал, сироты чисты, попытка одна за тик"

say "фаза 14: env-gc #1431 — observe логирует кандидатов, live сносит по вердикту мока"

# env-gc: candidates из снапшота доски (терминальная + закрытие > 48 ч),
# снос — bootstrap в чекауте + slash-команда task-env-gc, вердикт —
# user-сообщение мока (командный канал эмулирует shell-исход).
# 2060 закрыта 3 суток назад (кандидат), 2061 — сегодня (свежая);
# done-сиды CUP не занимают, тик терминальные строки не спавнит
CONFIG_EGC="$RUN_DIR/config.egc.toml"
cat > "$CONFIG_EGC" <<EOF
mode = "live"
poll_sec = 10
sweep_days = 7
env_gc_days = 1
env_gc_observe = false
port = $PORT_SRV

[hub]
repo = "yurvon-screamo/uwuwu-hub"
driver = "memory"

[executor]
name = "local-mock"
url = "http://127.0.0.1:$PORT_MOCK"

[policy]
cup = 1
peak = { enabled = false, start_min = 0, end_min = 0 }

[[project]]
name = "uwuwu"
board = "Задачи uwuwu"
repo = "yurvon-screamo/uwuwu-agent"
checkout = "/home/yurvon/uwuwu-agent"
env_root = "/home/yurvon/agents-env"
EOF
EGC_SEED="Задачи uwuwu:2060:Done:low:3;Задачи uwuwu:2061:Done:low:0"

# 14a: observe-гейт (env_gc_observe = true): кандидаты в логе, команд нет
sed 's/env_gc_observe = false/env_gc_observe = true/' "$CONFIG_EGC" > "$CONFIG_EGC.obs"
rm -f "$STATE"
LOG_SRV_EGC="$RUN_DIR/server-egc.log"
MOCK_MODE=idle MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=100 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG="$CONFIG_EGC.obs" UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$EGC_SEED" RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_EGC" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 10 ]; do
  SEEN=$((SEEN + 1)); sleep 4
  grep -q '"event":"env.gc.observe"' "$LOG_SRV_EGC" && break
done
grep -q '"event":"env.gc.observe"' "$LOG_SRV_EGC" \
  || fail "observe-env-gc не показал кандидатов (env.gc.observe нет за 40с)"
grep -q 'task-env-gc' "$LOG_MOCK" \
  && fail "observe-env-gc диспатчит команды (мутация в observe)"
say "14a ок: observe показал кандидата, команд нет"

# 14b: live — диспатч сноса, вердикт SWEPT через мок, свежая строка не тронута
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
rm -f "$STATE"
MOCK_MODE=idle MOCK_PORT=$PORT_MOCK MOCK_STEP_MS=100 \
  "$BIN_MOCK" >"$LOG_MOCK" 2>&1 &
MOCK_PID=$!
sleep 1
UWUWU_SERVER_CONFIG="$CONFIG_EGC" UWUWU_SERVER_STATE=$STATE PORT=$PORT_SRV \
  UWUWU_SERVER_BEARER="$BEARER" UWUWU_OPENCODE_PASSWORD=mockpass \
  UWUWU_MOCK_SEED="$EGC_SEED" RUST_LOG=info "$BIN_SRV" >"$LOG_SRV_EGC" 2>&1 &
SRV_PID=$!
SEEN=0
while [ $SEEN -lt 15 ]; do
  SEEN=$((SEEN + 1)); sleep 4
  grep -q '"event":"env.swept"' "$LOG_SRV_EGC" && break
done
grep -q '"event":"env.swept"' "$LOG_SRV_EGC" \
  || fail "live-env-gc не получил вердикт SWEPT (env.swept нет за 60с)"
grep -q '"event":"mock.command"' "$LOG_MOCK" && grep -q 'task-env-gc' "$LOG_MOCK" \
  || fail "мок не принял команду task-env-gc"
grep -q '"event":"env.gc.completed"' "$LOG_SRV_EGC" \
  || fail "нет итогового env.gc.completed"
# свежая строка (2061) не кандидат: счётчики completed сходятся
COMPLETED=$(grep '"event":"env.gc.completed"' "$LOG_SRV_EGC" | tail -1)
echo "$COMPLETED" | grep -q '"swept":1' || fail "completed без swept=1: $COMPLETED"
echo "$COMPLETED" | grep -q '"candidates":1' \
  || fail "completed насчитал не одного кандидата: $COMPLETED"
grep -q '"event":"mock.session_deleted"' "$LOG_MOCK" \
  || fail "bootstrap env-gc не погашен после прохода"
kill "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
wait "$SRV_PID" "$MOCK_PID" 2>/dev/null || true
SRV_PID=""; MOCK_PID=""
sleep 1
say "фаза 14 ок: observe без мутаций, live снёс кандидата по вердикту мока, свежая не тронута, bootstrap погашен"

say "ИТОГ: E2E зелёный (observe-чистота + полный цикл с ре-энтри и семейство-непрерывностью сессий #1237 + мульти-экзекьютор + гард #1163 + self-эхо в прод-геометрии #1237 + слепой hold #1263 + преемпшен CUP #1259 + давка-волна #1412 + клин #1293 + дример #1306 + phantom-квитанции #1331 + lost-сессия #1288: десинк-парковка + lost-resurrection + env-gc #1431); логи: $RUN_DIR"
