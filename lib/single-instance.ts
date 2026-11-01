// lib/single-instance.ts — лок-файл на один живой инстанс демона.
// mkdir-лок: атомарный захват; pid-проверка отличает живого владельца
// от мёртвого lock'а (после него перехватываем).
// Сигналы: onStop даёт демону доехать критическую секцию и выйти самому в
// безопасной точке; вынужденный process.exit — только по истечении grace,
// чтобы systemd не прибивал зависший drainage вечно.
//
// NB: cron/factory/dreamer пока несут собственные копии этой логики —
// миграция их на общий модуль запланирована отдельно (не в срезе triage).

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"

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
