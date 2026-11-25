#!/usr/bin/env bash
# task-env — подготовка окружения задачи на исполнителе (сервер вызывает
# через slash-команду task-env: shell-блок исполняется ДО промпта, без LLM).
# Идемпотентен: существующий env переиспользуется — рефсы origin подтягиваются,
# рабочее состояние ворктри не трогается. Рефреш кода — руками агента.
# Схема канона: ~/agents-env/<row>/{repo,scratch,logs}, ветка work/task-<row>.
set -euo pipefail

row="${1:?usage: task-env <row-id>}"
safe=$(printf '%s' "$row" | sed -E 's/^-+//; s/[^a-zA-Z0-9_-]//g')
[ -n "$safe" ] || { echo "task-env: некорректный row-id: $row" >&2; exit 1; }

env_root="${UWUWU_ENV_ROOT:-$HOME/agents-env}"
env_dir="$env_root/$safe"
repo="$env_dir/repo"
branch="work/task-$safe"

mkdir -p "$env_dir/scratch" "$env_dir/logs"

# .git — файл у ворктри, каталог у чекаута: проверяем наличие git-носителя.
if [ -e "$repo/.git" ] && git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  # переиспользование: подтянуть рефсы, не трогая рабочее состояние
  git -C "$repo" fetch -q origin || true
else
  # worktree от текущего мастер-чекаута (cwd команды = чекаут проекта)
  src="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  # гигиена stale-регистраций (env снесён, регистрация осталась)
  git -C "$src" worktree prune 2>/dev/null || true
  git -C "$src" fetch -q origin
  base=$(git -C "$src" rev-parse --verify origin/HEAD 2>/dev/null \
    || git -C "$src" rev-parse --verify origin/master \
    || git -C "$src" rev-parse --verify origin/main)
  if git show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$src" worktree add -q "$repo" "$branch"
  else
    git -C "$src" worktree add -q -b "$branch" "$repo" "$base"
  fi
fi

echo "env ready: $repo (branch $branch)"
