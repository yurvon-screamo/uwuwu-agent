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

/** Is the state directory sandboxed (env override set, non-empty)?
 *  Dry-run guard basis: fake/partial state writes must never land in prod
 *  state, so dry-run refuses to start on the default directory. */
export function sandboxedStateDir(envRaw: string | undefined): boolean {
  const v = envRaw?.trim()
  return v !== undefined && v !== ""
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
