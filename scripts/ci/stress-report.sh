#!/usr/bin/env bash
# Сборка сигнала стресс-джобы (stress-tests.yml): имена упавших тестов и
# фрагменты паник из логов упавших прогонов → markdown для
# $GITHUB_STEP_SUMMARY + ::error аннотации.
#
# Урок 10.11 (безымянный флейк полного прогона): имя теста фиксируется
# в момент падения автоматически — «безымянных fail» больше нет.
#
# Использование: stress-report.sh <logs-dir>
#   logs-dir содержит run-<N>.log (полный вывод cargo test) и
#   run-<N>.rc (exit-код прогона, пишет workflow).
set -euo pipefail

logs_dir="${1:?usage: stress-report.sh <logs-dir>}"
[ -d "$logs_dir" ] || { echo "stress-report: нет директории $logs_dir" >&2; exit 1; }

found=0
annotation_count=0

for log in "$logs_dir"/run-*.log; do
  [ -e "$log" ] || continue
  rc_file="${log%.log}.rc"
  [ -e "$rc_file" ] || continue
  rc="$(cat "$rc_file")"
  [ "$rc" -ne 0 ] || continue
  found=1
  run_id="$(basename "$log" .log)"

  # Имена упавших тестов — прямые строки "test X ... FAILED" из вывода.
  failed_tests="$(grep ' FAILED$' "$log" | sed 's/^test //; s/ \.\.\. FAILED$//' || true)"

  echo "## $run_id: FAIL (rc=$rc)"
  echo

  if [ -n "$failed_tests" ]; then
    echo '### Упавшие тесты'
    echo
    echo '```'
    echo "$failed_tests"
    echo '```'
    echo
  else
    # rc != 0 без FAILED-строк: падение компиляции или инфраструктура —
    # тоже сигнал, но другой природы.
    echo '### Упавших тестов не найдено — сбой компиляции/инфраструктуры'
    echo
  fi

  # Фрагменты паник: строка "thread ... panicked at ..." + сообщение.
  panics="$(grep -A1 '^thread .* panicked at ' "$log" | head -50 || true)"
  if [ -n "$panics" ]; then
    echo '### Паники'
    echo
    echo '```'
    echo "$panics"
    echo '```'
    echo
  fi

  # Аннотации: лимит GitHub — 10 error-аннотаций на шаг, полный список
  # живёт в summary.
  if [ "$annotation_count" -lt 10 ]; then
    if [ -n "$failed_tests" ]; then
      echo "::error::$run_id: упали: $(echo "$failed_tests" | head -5 | tr '\n' ' ')"
    else
      echo "::error::$run_id: сбой без упавших тестов (компиляция/инфраструктура)"
    fi
    annotation_count=$((annotation_count + 1))
  fi
done

if [ "$found" -eq 0 ]; then
  echo 'stress-report: упавших прогонов не найдено (джоба красная по другой причине)' >&2
fi
