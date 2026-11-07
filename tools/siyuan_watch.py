#!/usr/bin/env python3
"""siyuan_watch — watchdog-итерация контейнера siyuan-demo (quadlet-юнит).

Одна итерация (запуск systemd user-таймером каждую минуту):
  1. TCP-проба 127.0.0.1:6806 (транспорт-детект, timeout 3s);
  2. проба fail → state-факты контейнера (`podman inspect`), инцидент-строка
     в ~/.local/state/siyuan-watch/watch.log — JSON с фиксированными полями
     {ts, event, container_status, exit_code, restarts} — затем
     `systemctl --user start siyuan-demo.service` (юнит active — no-op,
     безопасно; хил именно через systemd: контейнер под quadlet пересоздаётся
     при каждом старте, снаружи поднимать нельзя — сломает связку с юнитом);
  3. верификационная перепроба: поднялся → exit 0, нет → exit 1 (деталь —
     в stderr, виден в journalctl siyuan-watch.service).

Лог = история инцидентов, не телеметрия: OK-тики в лог не пишутся. Строка с
container_status="running" при мёртвом порте — сигнатура hang-класса
(pasta-клин / вис туннеля): вотчер её только фиксирует, лечение хангов —
отдельная лестница (осознанно вне среза, задача 20261105224609).

Механика класса «тихие остановки» и рецепт атрибуции SIGTERM —
devops/«SiYuan-контур uwuwu», режим 8. Строки-инциденты читаются по полям,
не grep'ом по тексту (rules-logging: event — стабильный словарь).
"""
from __future__ import annotations

import datetime
import json
import socket
import subprocess  # nosec B404 — назначение скрипта: запуск podman inspect и systemctl start с фиксированными аргументами
import sys
import time
from pathlib import Path
from typing import Callable

PROBE_HOST = "127.0.0.1"
PROBE_PORT = 6806
PROBE_TIMEOUT_S = 3
CONTAINER = "siyuan-demo"
SERVICE_UNIT = f"{CONTAINER}.service"
LOG_FILE = Path.home() / ".local" / "state" / "siyuan-watch" / "watch.log"
# systemctl start возвращается при старте контейнера (Type=notify/conmon) —
# слушателю ядра нужен короткий грейс, иначе медленный бут даёт ложный
# «failed to heal» (ревью PR #85, Low#1)
HEAL_GRACE_S = 3.0

EVENT_DOWN = "container_down"

Runner = Callable[..., subprocess.CompletedProcess]
Probe = Callable[[], bool]


def probe_tcp(host: str = PROBE_HOST, port: int = PROBE_PORT, timeout_s: float = PROBE_TIMEOUT_S) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout_s):
            return True
    except OSError:
        return False


def _missing_state() -> dict:
    return {"container_status": "missing", "exit_code": None, "restarts": None}


def read_container_state(container: str = CONTAINER, runner: Runner = subprocess.run) -> dict:
    """State-факты из `podman inspect`: статус, exit-код последнего выхода, счётчик рестартов.

    Под quadlet контейнер пересоздаётся юнитом при каждом старте (--rm), поэтому
    «missing» при дауне — норма миграции, а не деградация; сигнатура hang-класса
    (running + мёртвый порт) остаётся различимой.
    """
    try:
        proc = runner(["podman", "inspect", container, "--format", "{{json .}}"], capture_output=True, text=True)
    except OSError:
        return _missing_state()
    if proc.returncode != 0:
        return _missing_state()
    try:
        facts = json.loads(proc.stdout)
    except ValueError:
        return _missing_state()
    state = facts.get("State", {})
    return {
        "container_status": state.get("Status", "unknown"),
        "exit_code": state.get("ExitCode"),
        "restarts": facts.get("RestartCount"),
    }


def start_container(unit: str = SERVICE_UNIT, runner: Runner = subprocess.run) -> tuple[bool, str]:
    """`systemctl --user start`: на active-юните — no-op (exit 0).

    Хил только через systemd: контейнер под quadlet пересоздаётся юнитом
    (`podman run --replace --rm` на каждый старт), внешний `podman start`
    поднял бы его мимо юнита и сломал связку (конфликт имён на следующем
    рестарте юнита).
    """
    try:
        proc = runner(["systemctl", "--user", "start", unit], capture_output=True, text=True)
    except OSError as error:
        return False, str(error)
    if proc.returncode == 0:
        return True, ""
    return False, (proc.stderr or proc.stdout).strip()


def incident_record(state: dict, now: Callable[[], datetime.datetime] | None = None) -> dict:
    moment = (now or datetime.datetime.now)().astimezone()
    return {
        "ts": moment.isoformat(timespec="seconds"),
        "event": EVENT_DOWN,
        "container_status": state.get("container_status"),
        "exit_code": state.get("exit_code"),
        "restarts": state.get("restarts"),
    }


def write_incident(log_path: Path, record: dict) -> None:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("a", encoding="utf-8") as log:
        log.write(json.dumps(record, ensure_ascii=False) + "\n")


def run_once(
    probe: Probe | None = None,
    read_state: Callable[[], dict] | None = None,
    start: Callable[[], tuple[bool, str]] | None = None,
    log_path: Path = LOG_FILE,
    sleeper: Callable[[float], None] = time.sleep,
    heal_grace_s: float = HEAL_GRACE_S,
    stderr=None,
) -> int:
    """Одна watchdog-итерация: 0 — здоров/вылечен, 1 — вниз и не подняли."""
    probe = probe or (lambda: probe_tcp())
    read_state = read_state or (lambda: read_container_state())
    start = start or (lambda: start_container())
    stderr = stderr or sys.stderr

    if probe():
        return 0

    write_incident(log_path, incident_record(read_state()))
    started, failure = start()
    if not started:
        print(f"failed to start container {CONTAINER} due to {failure}", file=stderr)
        return 1
    sleeper(heal_grace_s)
    if probe():
        print(f"started container {CONTAINER} after watchdog incident", file=stderr)
        return 0
    print(f"failed to heal container {CONTAINER}: port still down after start", file=stderr)
    return 1


def main() -> int:
    return run_once()


if __name__ == "__main__":
    sys.exit(main())
