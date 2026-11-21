#!/usr/bin/env bash
# task-env-gc — снос env закрытой задачи на исполнителе (#1431). Сервер
# вызывает через slash-команду task-env-gc (shell-блок до промпта, без
# LLM) по строке-кандидату из снапшота доски; решение о кандидате
# (терминальность, возраст закрытия, сессии) — на сервере, скрипт несёт
# гарды ФС. Вердикты в STDOUT строками «ENV-GC <row> <verdict>» —
# сервер ждёт строку с маркером «ENV-GC <row>» в message-ленте
# bootstrap-сессии. Порядок сноса — ADR-041: worktree remove → rm -rf
# env → prune → branch -d.
#
# Гард-цепочка (первый отказ — вердикт без мутаций):
#   ABSENT  — env-каталога нет (уже снесён вручную/ранее);
#   NOREPO  — repo/.git отсутствует (чистота и ownership недоказуемы —
#             автоснос исключён, ручной разбор);
#   FOREIGN — ворктри не зарегистрирован в чекауте проекта (cwd вызова):
#             env_root общий у нескольких проектов, чужое не трогаем;
#   DIRTY   — git status --porcelain непуст (незакоммиченная работа);
#   SWEPT   — env снесён; FAIL — мутация не доведена до конца.
set -euo pipefail

cmd="${1:-}"
row="${2:-}"
if [ "$cmd" != "sweep" ] || [ -z "$row" ]; then
  echo "usage: task-env-gc.sh sweep <row-id>" >&2
  exit 1
fi

safe=$(printf '%s' "$row" | sed -E 's/^-+//; s/[^a-zA-Z0-9_-]//g')
[ -n "$safe" ] || { echo "ENV-GC $row FAIL (некорректный row-id)"; exit 0; }

env_root="${UWUWU_ENV_ROOT:-$HOME/agents-env}"
env_dir="$env_root/$safe"
repo="$env_dir/repo"
branch="work/task-$safe"

if [ ! -d "$env_dir" ]; then
  echo "ENV-GC $safe ABSENT"
  exit 0
fi
if [ ! -e "$repo/.git" ]; then
  echo "ENV-GC $safe NOREPO"
  exit 0
fi

# Ownership: ворктри обязан быть зарегистрирован в чекауте проекта
# (cwd команды = чекаут, как у task-env). Путь сверяется канонически —
# расхождение (ручной каталог, другой проект) = FOREIGN.
src="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
if ! git -C "$src" worktree list --porcelain | grep -Fxq "worktree $repo"; then
  echo "ENV-GC $safe FOREIGN"
  exit 0
fi

# Чистота: незакоммиченная работа, включая untracked, блокирует снос.
if [ -n "$(git -C "$repo" status --porcelain)" ]; then
  echo "ENV-GC $safe DIRTY"
  exit 0
fi

# worktree remove без --force: porcelain пуст проверен выше — ignored
# (target/node_modules) уходит сам, вторая грязь-защита остаётся живой.
if ! git -C "$src" worktree remove "$repo"; then
  echo "ENV-GC $safe FAIL (worktree remove)"
  exit 0
fi
rm -rf "$env_dir" || { echo "ENV-GC $safe FAIL (rm -rf)"; exit 0; }
git -C "$src" worktree prune || true
# Строчный -d отказывается на несмерженной ветке: ветка переживает снос
# (refs не диск), отказ — заметка серверу, вердикт не портит.
if ! git -C "$src" branch -d "$branch" >/dev/null 2>&1; then
  echo "task-env-gc: ветка $branch переживает снос (не смержена?)"
fi
if [ -e "$env_dir" ]; then
  echo "ENV-GC $safe FAIL (rm -rf не довёл)"
  exit 0
fi
echo "ENV-GC $safe SWEPT"
