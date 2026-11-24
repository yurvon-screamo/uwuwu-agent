#!/usr/bin/env bash
# task-env-gc — снос env закрытых задач на исполнителе (#1431, батч
# #1526). Сервер вызывает через slash-команду task-env-gc ОДНОЙ командой
# на проход со списком строк-кандидатов (args через пробел, opencode
# резолвит $1 = весь хвост args — tail-join старшего placeholder, проба
# #1526); решение о кандидате (терминальность, возраст закрытия,
# сессии) — на сервере, скрипт несёт гарды ФС и циклит строки сам.
# Вердикты в STDOUT строками «ENV-GC <row> <verdict>» — по строке на
# каждую строку батча; все они доезжают одним user-сообщением в ленте
# bootstrap-сессии, сервер собирает их parse-all'ом. Порядок сноса —
# ADR-041: worktree remove → rm -rf env → prune → branch -d.
#
# Гард-цепочка (первый отказ — вердикт без мутаций), per-row:
#   ABSENT  — env-каталога нет (уже снесён вручную/ранее);
#   NOREPO  — repo/.git отсутствует (чистота и ownership недоказуемы —
#             автоснос исключён, ручной разбор);
#   FOREIGN — ворктри не зарегистрирован в чекауте проекта (cwd вызова):
#             env_root общий у нескольких проектов, чужое не трогаем;
#   DIRTY   — git status --porcelain непуст (незакоммиченная работа);
#   SWEPT   — env снесён; FAIL — мутация не доведена до конца.
#
# Контракт per-row функции: страховка цикла `|| echo FAIL-вердикт`
# ловит только не-ноль статуса самой функции (явный return ≠ 0; `||`
# гасит set -e внутри тела — упавшая команда функцию не прерывает, все
# guard-ветки ниже явные). exit внутри функции запрещён: унёс бы весь
# батч, потеряв вердикты остальных строк.
set -euo pipefail

cmd="${1:-}"
shift || true
if [ "$cmd" != "sweep" ] || [ "$#" -eq 0 ]; then
  echo "usage: task-env-gc.sh sweep <row-id> [<row-id>...]" >&2
  exit 1
fi

safe_row() {
  printf '%s' "$1" | sed -E 's/^-+//; s/[^a-zA-Z0-9_-]//g'
}

sweep_one() {
  local row="$1"
  local safe
  safe=$(safe_row "$row")
  if [ -z "$safe" ]; then
    echo "ENV-GC $row FAIL (некорректный row-id)"
    return 0
  fi

  local env_root="${UWUWU_ENV_ROOT:-$HOME/agents-env}"
  local env_dir="$env_root/$safe"
  local repo="$env_dir/repo"
  local branch="work/task-$safe"

  if [ ! -d "$env_dir" ]; then
    echo "ENV-GC $safe ABSENT"
    return 0
  fi
  if [ ! -e "$repo/.git" ]; then
    echo "ENV-GC $safe NOREPO"
    return 0
  fi

  # Ownership: ворктри обязан быть зарегистрирован в чекауте проекта
  # (cwd команды = чекаут, как у task-env). Путь сверяется канонически —
  # расхождение (ручной каталог, другой проект) = FOREIGN.
  local src
  src="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  if ! git -C "$src" worktree list --porcelain | grep -Fxq "worktree $repo"; then
    echo "ENV-GC $safe FOREIGN"
    return 0
  fi

  # Чистота: незакоммиченная работа, включая untracked, блокирует снос.
  if [ -n "$(git -C "$repo" status --porcelain)" ]; then
    echo "ENV-GC $safe DIRTY"
    return 0
  fi

  # worktree remove без --force: porcelain пуст проверен выше — ignored
  # (target/node_modules) уходит сам, вторая грязь-защита остаётся живой.
  if ! git -C "$src" worktree remove "$repo"; then
    echo "ENV-GC $safe FAIL (worktree remove)"
    return 0
  fi
  rm -rf "$env_dir" || { echo "ENV-GC $safe FAIL (rm -rf)"; return 0; }
  git -C "$src" worktree prune || true
  # Строчный -d отказывается на несмерженной ветке: ветка переживает снос
  # (refs не диск), отказ — заметка серверу, вердикт не портит.
  if ! git -C "$src" branch -d "$branch" >/dev/null 2>&1; then
    echo "task-env-gc: ветка $branch переживает снос (не смержена?)"
  fi
  if [ -e "$env_dir" ]; then
    echo "ENV-GC $safe FAIL (rm -rf не довёл)"
    return 0
  fi
  echo "ENV-GC $safe SWEPT"
  return 0
}

# Цикл батча: страховка `||` — на не-ноль статуса per-row функции
# (явный return ≠ 0; set -e внутри тела гасится — см. контракт выше).
# В текущем теле не-ноль недостижим (все исходы явные) — страховка на
# будущие правки; фикс-кейс FAIL живёт в ветке `if ! git worktree remove`.
for row in "$@"; do
  sweep_one "$row" || echo "ENV-GC $(safe_row "$row") FAIL (внезапный падеж строки)"
done
