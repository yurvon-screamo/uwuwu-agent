// lib/single-instance.ts — лок-файл на один живой инстанс демона.
// mkdir-лок: атомарный захват; pid-проверка отличает живого владельца
// от мёртвого lock'а (после него перехватываем).
// Сигналы: onStop даёт демону доехать критическую секцию и выйти самому в
// безопасной точке; вынужденный process.exit — только по истечении grace,
// чтобы systemd не прибивал зависший drainage вечно.
//
// Пользователи: triage, cron, factory, dreamer — свои копии лока удалены.

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"

/** Resolves the daemon state directory: a non-empty env override wins,
 *  otherwise the historical default (~/.local/state/<daemon>).
 *  Empty/whitespace env counts as unset — a sandbox var left empty must not
 *  silently point the daemon (and its lock) at a bogus relative path. */
export function resolveStateDir(envRaw: string | undefined, fallback: string): string {
  const v = envRaw?.trim()
  return v !== undefined && v !== "" ? v : fallback
}

/** Ветка git-чекаута: `git branch --show-current` (не следует за runtime-
 *  подменой окружения — читает реальный чекаут по repoRoot). Пустой stdout —
 *  detached HEAD; не-git-каталог/ошибка спавна — null (детект не удался).
 *  Инъекция в тестах — resolveDaemonStateDir({ detect }). */
export function detectBranch(repoRoot: string): string | null {
  try {
    const p = Bun.spawnSync(["git", "branch", "--show-current"], { cwd: repoRoot, stdout: "pipe", stderr: "pipe" })
    if (p.exitCode !== 0) return null
    return new TextDecoder().decode(p.stdout)
  } catch {
    return null
  }
}

/** Имя ветки → безопасный сегмент пути (межплатформенно: срезаются и слэши
 *  work/task-x, и двоеточия windows-веток). */
function branchSegment(branch: string): string {
  return branch.replace(/[^A-Za-z0-9._-]/g, "_")
}

/** Резолюция state-каталога демона с дефолт-изоляцией веточных чекаутов
 *  (инцидент-класс 06.11: прогон демона из ветки без env читал и писал
 *  общий прод-state). Порядок:
 *  1. явный env — as-is, песочница (механика PR #71);
 *  2. master — прод-дефолт, прежнее поведение прод-демона;
 *  3. не-master — авто-изоляция <defaultDir>/branches/<ветка>: свой state+lock,
 *     прод-state не читается и не пишется, лог тоже переезжает в песочницу;
 *  4. git-детект не удался — прод-дефолт + громкий warning: прод-демон не
 *     должен зависеть от git-окружения, а ложная изоляция прода (пустой
 *     state → волна re-spawn) страшнее честного warning'а. */
export interface DaemonStateResolution {
  stateDir: string
  /** state изолирован от прода: явный env ИЛИ авто-изоляция ветки */
  sandboxed: boolean
  /** авто-изоляция: не-master чекаут без явного env */
  autoIsolated: boolean
  /** громкое замечание к стартовому логу (console.error + log); null — тихо */
  notice: string | null
}

export function resolveDaemonStateDir(opts: {
  /** имя демона для контекста notice (напр. «uwuwu-factory») */
  daemon: string
  /** имя env-переменной — подсказка в notice, как уйти от дефолта */
  envName: string
  envRaw: string | undefined
  /** прод-дефолт (~/.local/state/<daemon>), от него же считаются branches/ */
  defaultDir: string
  /** корень чекаута демона (репо с исходниками) */
  repoRoot: string
  /** инъекция для тестов; дефолт — реальный detectBranch */
  detect?: (repoRoot: string) => string | null
}): DaemonStateResolution {
  const env = opts.envRaw?.trim()
  if (env !== undefined && env !== "") {
    return { stateDir: env, sandboxed: true, autoIsolated: false, notice: null }
  }
  const detect = opts.detect ?? detectBranch
  const raw = detect(opts.repoRoot)
  if (raw === null) {
    return {
      stateDir: opts.defaultDir,
      sandboxed: false,
      autoIsolated: false,
      notice: `${opts.daemon}: git-детект ветки не удался (${opts.repoRoot}) — работаю на дефолтном state ${opts.defaultDir}; если это веточный чекаут, задай ${opts.envName} — иначе прод-state будет перезаписан`,
    }
  }
  const branch = raw.trim()
  if (branch === "master") {
    return { stateDir: opts.defaultDir, sandboxed: false, autoIsolated: false, notice: null }
  }
  const label = branch === "" ? "detached" : branch
  const stateDir = join(opts.defaultDir, "branches", branchSegment(label))
  return {
    stateDir,
    sandboxed: true,
    autoIsolated: true,
    notice: `${opts.daemon}: чекаут «${label}» — не master: state изолирован в ${stateDir} (прод-state не читается и не пишется); осознанный прод из ветки — задай ${opts.envName}`,
  }
}

export function acquireLock(opts: {
  stateDir: string
  onStop: () => void
  graceMs?: number
}): boolean {
  const lockDir = join(opts.stateDir, "lock")
  mkdirSync(opts.stateDir, { recursive: true })
  try {
    mkdirSync(lockDir, { recursive: false })
  } catch {
    try {
      const pid = Number(readFileSync(join(lockDir, "pid"), "utf-8").trim())
      if (Number.isInteger(pid)) {
        process.kill(pid, 0)
        return false // живой владелец
      }
    } catch { /* мёртвый lock — перехватываем */ }
    try {
      rmSync(lockDir, { recursive: true, force: true })
      mkdirSync(lockDir, { recursive: false })
    } catch {
      return false
    }
  }
  writeFileSync(join(lockDir, "pid"), String(process.pid))
  // лок держим до реального выхода процесса (включая graceful drain):
  // чистим на exit — после останова не остаётся stale-lock
  process.once("exit", () => {
    try { rmSync(lockDir, { recursive: true, force: true }) } catch {}
  })
  const stopped = { v: false }
  const stop = (): void => {
    if (stopped.v) return
    stopped.v = true
    opts.onStop()
    setTimeout(() => process.exit(0), opts.graceMs ?? 15_000).unref()
  }
  process.on("SIGTERM", stop)
  process.on("SIGINT", stop)
  return true
}

/** Drain-примитив тик-демонов: флаг останова + прерываемый сон.
 *  onStop лока взводит флаг; тик доезжает, цикл видит isStopping() и выходит
 *  сам. sleep() режет интервал на шаги по 0.5с — SIGTERM между тиками даёт
 *  чистый выход за шаг, а не через grace-килл (оптимизация латентности). */
export interface Drain {
  /** Взвести флаг останова (передавать как onStop лока). */
  stop(): void
  /** Флаг останова взведён — циклу пора на чистый выход. */
  isStopping(): boolean
  /** Сон до totalMs с выходом сразу после взведения флага. */
  sleep(totalMs: number): Promise<void>
}

export function makeDrain(): Drain {
  let stopping = false
  return {
    stop: (): void => { stopping = true },
    isStopping: (): boolean => stopping,
    sleep: async (totalMs: number): Promise<void> => {
      for (let waited = 0; waited < totalMs && !stopping; waited += 500) {
        await new Promise((r) => setTimeout(r, Math.min(500, totalMs - waited)))
      }
    },
  }
}
