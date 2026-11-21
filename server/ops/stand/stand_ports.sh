#!/usr/bin/env bash
# stand_ports.sh — подбор свободных портов для стендов server/ops/stand (#1304).
#
# Проба-бинд 127.0.0.1:0: ОС выдаёт свободный ephemeral-порт, сокет сразу
# закрывается. Семантика скриптов стенда: оверрайд-переменная задана — порт
# берётся как есть (занятость на ответственности зовущего, отказ бинда
# громкий); не задана — значение из stand_pick_ports. Окно между пробой и
# реальным биндом процесса — принятый TOCTOU-трейдофф (#1304): остаточное
# столкновение даёт громкий bind-отказ, не маскированную панику.
#
# Использование:
#   source "$(dirname "$0")/stand_ports.sh"
#   ports="$(stand_pick_ports 3)"   # отказ подбора роняет скрипт: set -e
#                                   # ловит статус присваивания (read в
#                                   # подстановке — нет)
#   read -r A B C <<<"$ports"
#   PORT_X="${PORT_X:-$A}"

# stand_pick_ports N — N различных свободных портов одной строкой (stdout)
stand_pick_ports() {
  local n="${1:?stand_pick_ports: нужен аргумент — сколько портов}"
  python3 - "$n" <<'PY'
import socket, sys
n = int(sys.argv[1])
picked = set()
while len(picked) < n:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    picked.add(s.getsockname()[1])
    s.close()
print(" ".join(str(p) for p in sorted(picked)))
PY
}
