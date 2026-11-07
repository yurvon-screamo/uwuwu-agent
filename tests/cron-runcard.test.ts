// tests/cron-runcard.test.ts — позитивный путь createRunCard (ревью Low):
// парсинг rowId из stdout `task create` («строка: <id>») и вторичный спавн
// `task edit <id> --status Todo`. Дрейф формата вывода CLI иначе ломал бы
// создание ранов молча (только строка в логе). Bun.spawn патчится на скрипт:
// createRunCard экспортирована из демона (паттерн tickBoard).
import { afterEach, describe, expect, test } from "bun:test"

// CLI_BIN печётся на импорте демона: фиксируем до import — ассерты не зависят
// от наличия ~/.cargo/bin/uwuwu-cli на машине прогона
process.env.UWUWU_CLI_BIN = "uwuwu-cli-test-mock"

const cron = await import("../cron/cron")

interface SpawnCall {
  argv: string[]
  stdinWritten: string
}

interface SpawnScript {
  stdout: string
  stderr?: string
  code?: number
}

const originalSpawn = Bun.spawn

function scriptSpawn(scripts: SpawnScript[]): { calls: SpawnCall[] } {
  const calls: SpawnCall[] = []
  let i = 0
  ;(Bun as unknown as { spawn: unknown }).spawn = ((argv: string[], opts?: { stdin?: string }) => {
    const script = scripts[i] ?? { stdout: "", code: 0 }
    i += 1
    const call: SpawnCall = { argv, stdinWritten: opts?.stdin === "pipe" ? "" : "(не пайп)" }
    calls.push(call)
    const proc = {
      stdin: {
        write: (chunk: string) => {
          call.stdinWritten = chunk
        },
        end: () => {},
      },
      stdout: new Response(script.stdout).body,
      stderr: new Response(script.stderr ?? "").body,
      exited: Promise.resolve(script.code ?? 0),
    }
    return proc as unknown as ReturnType<typeof Bun.spawn>
  }) as typeof Bun.spawn
  return { calls }
}

afterEach(() => {
  ;(Bun as unknown as { spawn: unknown }).spawn = originalSpawn
})

describe("createRunCard: позитивный путь", () => {
  test("парсит rowId из stdout create и переводит ран в Todo", async () => {
    const { calls } = scriptSpawn([
      { stdout: "создана: ⏰ мастер · 2026-11-07 09:30:00\n  строка: 20261107090000-run001\n  тело:   20261107090000-body01\n" },
      { stdout: "обновлено: ⏰ мастер · 2026-11-07 09:30:00\n" },
    ])

    const rowId = await cron.createRunCard(
      "uwuwu",
      "⏰ мастер · 2026-11-07 09:30:00",
      "Ран крон-задачи «мастер».\n\n## Инструкция мастера\nпроверь",
    )

    expect(rowId).toBe("20261107090000-run001")
    expect(calls.length).toBe(2, "create + edit")
    const [createCall, editCall] = calls
    // create: заголовок и тело через stdin (argv[0] — бинарь, путь зависит
    // от машины/порядка импортов — не ассертим)
    expect(createCall?.argv.slice(1, 5)).toEqual(["task", "create", "--project", "uwuwu"])
    expect(createCall?.stdinWritten).toContain("## Инструкция мастера")
    // edit: тот же rowId, статус Todo + снятие типа-дефолта (ран не несёт тип;
    // stdin не пайп — тело не несёт)
    expect(editCall?.argv.slice(1)).toEqual([
      "task", "edit", "20261107090000-run001", "--project", "uwuwu", "--status", "Todo", "--type", "",
    ])
    expect(editCall?.stdinWritten).toBe("(не пайп)")
  })

  test("код create ≠ 0 → null без edit-спавна", async () => {
    const { calls } = scriptSpawn([
      { stdout: "", stderr: "живая строка с тайтлом «X» уже есть", code: 1 },
    ])

    const rowId = await cron.createRunCard("uwuwu", "⏰ дубль", "тело")
    expect(rowId).toBeNull()
    expect(calls.length).toBe(1, "edit после неудачного create не вызывается")
  })

  test("stdout без строки «строка: …» → null (дрейф формата не минтит раны)", async () => {
    const { calls } = scriptSpawn([
      { stdout: "создана: X\n(формат изменился: строки нет)\n" },
    ])

    const rowId = await cron.createRunCard("uwuwu", "⏰ X", "тело")
    expect(rowId).toBeNull()
    expect(calls.length).toBe(1, "edit не зовётся без rowId")
  })
})
