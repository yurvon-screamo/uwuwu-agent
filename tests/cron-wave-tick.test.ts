// tests/cron-wave-tick.test.ts — интеграция wave-класса с consecutiveFails
// крона (срез D задачи 20261107132538): KernelWaveError не считается в
// consecutiveFails и не рестартит демон на самозаживающейся волне —
// регрессионная точка: при TICK_SEC=60 и окне 90s прежний подсчёт давал
// exit(1) на 5-м тике. Чистая policy-функция — цикл не нужен.
import { describe, expect, test } from "bun:test"
import { KernelWaveError, KernelUnavailableError } from "../lib/platform"
import { tickFailOutcome } from "../cron/cron"

describe("tickFailOutcome — wave вне consecutiveFails", () => {
  const wave = (): unknown => new KernelWaveError("hub POST: пропущено — окно fail-fast активно")

  test("5 тиков подряд с wave — рестарта нет ни на одном", () => {
    let fails = 0
    for (let i = 0; i < 5; i++) {
      const outcome = tickFailOutcome(wave(), fails)
      expect(outcome.restart).toBe(false)
      expect(outcome.reconnect).toBe(false)
      expect(outcome.fails).toBe(fails)
      fails = outcome.fails
    }
    expect(fails).toBe(0)
  })

  test("обычный сбой копит счётчик и рестартит на 5-м", () => {
    let fails = 0
    for (let i = 0; i < 5; i++) {
      const outcome = tickFailOutcome(new Error("boom"), fails)
      fails = outcome.fails
      expect(outcome.reconnect).toBe(true)
      expect(outcome.restart).toBe(i === 4)
    }
  })

  test("wave между реальными фейлами счётчик не сбрасывает: отравленное соединение доедет до рестарта", () => {
    let fails = 0
    // 3 реальных сбоя
    for (let i = 0; i < 3; i++) fails = tickFailOutcome(new Error("boom"), fails).fails
    // wave-тик мимо счётчика
    const waveOutcome = tickFailOutcome(wave(), fails)
    expect(waveOutcome.fails).toBe(3)
    // 4-й реальный — без рестарта, 5-й — рестарт (wave не ускорил и не отменил)
    expect(tickFailOutcome(new Error("boom"), waveOutcome.fails).restart).toBe(false)
    expect(tickFailOutcome(new Error("boom"), 4).restart).toBe(true)
  })

  test("wave — подкласс KernelUnavailableError: ветвление factory/triage сохраняется", () => {
    expect(wave()).toBeInstanceOf(KernelUnavailableError)
  })
})
