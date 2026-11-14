// tests/cron-runcard.test.ts — позитивный путь createRunCard:
// парсинг rowId из stdout `task create` (контракт ответов: строка «Строка: <id>» шапки),
// начальный статус Todo в argv create (Draft-фазы рана нет — draftLoop триажа
// раны не видит) и вторичный спавн `task edit <id> --type "" --planned`.
// Дрейф формата вывода CLI иначе ломал бы создание ранов молча (только строка
// в логе). Bun.spawn патчится на скрипт: createRunCard экспортирована из
// демона (паттерн tickBoard).
import { afterEach, describe, expect, test } from "bun:test"

// CLI_BIN печётся на импорте демона: фиксируем до import — ассерты не зависят
// от наличия ~/.cargo/bin/uwuwu-cli на машине прогона
process.env.UWUWU_CLI_BIN = "uwuwu-cli-test-mock"

const cron = await import("../cron/cron")
const cronlib = await import("../cron/cron-lib")

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

describe("runCardBody: шапка ран-карточки", () => {
  test("шапка предписывает агенту закрыть успешный ран сам — без «Закрывает владелец»", () => {
    const body = cron.runCardBody("мастер X", "6h", "проверь hub", new Date(2026, 10, 7, 9, 30))
    expect(body).toContain("закрывает агент рана")
    expect(body).not.toContain("Закрывает владелец")
    expect(body).toContain("## Инструкция мастера")
    expect(body).toContain("проверь hub")
    expect(body).toContain("расписание: `6h`")
  })
})

describe("createRunCard: позитивный путь", () => {
  test("парсит rowId из stdout create, минтит ран сразу Todo (--status) и доставляет тип+planned edit'ом", async () => {
    const { calls } = scriptSpawn([
      { stdout: "Задача:   ⏰ мастер · 2026-11-07 09:30:00\nСтатус:   Todo\nТип:      feature\nСтрока:   20261107090000-run001\nДоска:    /uwuwu/Задачи uwuwu\n\n----- тело -----\n# ран\n" },
      { stdout: "Задача:   ⏰ мастер · 2026-11-07 09:30:00\nСтатус:   Todo\nСтрока:   20261107090000-run001\nДоска:    /uwuwu/Задачи uwuwu\n" },
    ])

    const rowId = await cron.createRunCard(
      "uwuwu",
      "⏰ мастер · 2026-11-07 09:30:00",
      "Ран крон-задачи «мастер».\n\n## Инструкция мастера\nпроверь",
      new Date(2026, 10, 7, 9, 30, 21),
    )

    expect(rowId).toBe("20261107090000-run001")
    // bun-types: .toBe(value) — 1-аргенная сигнатура, сообщение = название теста
    expect(calls.length).toBe(2)
    const [createCall, editCall] = calls
    // create: начальный статус Todo в argv (ран без Draft-фазы — draftLoop
    // триажа раны не видит); заголовок и тело через stdin (argv[0] — бинарь,
    // путь зависит от машины/порядка импортов — не ассертим)
    expect(createCall?.argv.slice(1, 5)).toEqual(["task", "create", "--project", "uwuwu"])
    expect(createCall?.argv.slice(-3)).toEqual(["--status", "Todo", "--stdin"])
    expect(createCall?.stdinWritten).toContain("## Инструкция мастера")
    // edit: тот же rowId, снятие типа-дефолта + дата файра
    // в «Запланировано» (минутная точность — формат сравнения reconcile);
    // статуса в edit больше нет — Todo уже записан create'ом
    expect(editCall?.argv.slice(1)).toEqual([
      "task", "edit", "20261107090000-run001", "--project", "uwuwu", "--type", "", "--planned", "2026-11-07 09:30",
    ])
    expect(editCall?.stdinWritten).toBe("(не пайп)")
  })

  test("код create ≠ 0 → null без edit-спавна", async () => {
    const { calls } = scriptSpawn([
      { stdout: "", stderr: "живая строка с тайтлом «X» уже есть", code: 1 },
    ])

    const rowId = await cron.createRunCard("uwuwu", "⏰ дубль", "тело", new Date(2026, 10, 7, 9, 30))
    expect(rowId).toBeNull()
    expect(calls.length).toBe(1)
  })

  test("stdout без строки «Строка: …» → null (дрейф формата не минтит раны)", async () => {
    const { calls } = scriptSpawn([
      { stdout: "Задача:   X\nСтатус:   Draft\n(формат изменился: строки нет)\n" },
    ])

    const rowId = await cron.createRunCard("uwuwu", "⏰ X", "тело", new Date(2026, 10, 7, 9, 30))
    expect(rowId).toBeNull()
    expect(calls.length).toBe(1)
  })

  test("hub-формат stdout create («создана: issue #N») — rowId = номер issue, edit идёт по нему", async () => {
    const { calls } = scriptSpawn([
      { stdout: "создана: issue #912 «⏰ QA · 2026-11-14 02:30:00» (Todo)\n" },
      { stdout: "изменена: issue #912 «⏰ QA · 2026-11-14 02:30:00»\n" },
    ])

    const rowId = await cron.createRunCard("true-washing", "⏰ QA · 2026-11-14 02:30:00", "тело", new Date(2026, 10, 14, 2, 30))
    expect(rowId).toBe("912")
    expect(calls.length).toBe(2)
    const [, editCall] = calls
    expect(editCall?.argv.slice(1)).toEqual([
      "task", "edit", "912", "--project", "true-washing", "--type", "", "--planned", "2026-11-14 02:30",
    ])
  })
})

describe("runRowIdFromCreateOutput: контракты форматов stdout", () => {
  const parse = cronlib.runRowIdFromCreateOutput
  test("hub create → номер issue", () => {
    expect(parse("создана: issue #912 «X» (Todo)\n")).toBe("912")
  })
  test("легаси create → «Строка: <rowId>» шапки", () => {
    expect(parse("Задача:   X\nСтатус:   Todo\nСтрока:   20261107090000-run001\n")).toBe("20261107090000-run001")
  })
  test("get-форма «Строка: issue #N» — не create-контракт, null (не минтить «issue» как id)", () => {
    expect(parse("Задача:   X\nСтрока:   issue #384\n")).toBeNull()
  })
  test("нераспознанный вывод → null", () => {
    expect(parse("")).toBeNull()
    expect(parse("какой-то дрейф формата\n")).toBeNull()
  })
  test("hub-формат приоритетен над легаси-веткой", () => {
    expect(parse("создана: issue #5 «Y» (Todo)\nСтрока:   20261107090000-run001\n")).toBe("5")
  })
})
