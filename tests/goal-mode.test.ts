// tests/goal-mode.test.ts — тесты чистых функций goal-mode (bun test)
import { test, expect, describe } from "bun:test"
import {
  parseGoalCommand,
  parseVerdict,
  renderTranscript,
  judgePrompt,
  lastMessageText,
  lastAssistantText,
  type TranscriptMessage,
} from "../lib/goal-lib"

describe("parseGoalCommand", () => {
  test("goal: с текстом — регистрирует цель", () => {
    expect(parseGoalCommand("goal: подготовь 3 поста")).toEqual({
      kind: "goal",
      text: "подготовь 3 поста",
    })
  })

  test("goal: без текста — не цель", () => {
    expect(parseGoalCommand("goal:")).toBeNull()
    expect(parseGoalCommand("goal:   ")).toBeNull()
  })

  test("goal-stop — снимает", () => {
    expect(parseGoalCommand("goal-stop")).toEqual({ kind: "stop" })
    expect(parseGoalCommand("  GOAL-STOP ")).toEqual({ kind: "stop" })
  })

  test("обычный текст — не команда", () => {
    expect(parseGoalCommand("просто сообщение")).toBeNull()
    expect(parseGoalCommand("")).toBeNull()
  })

  test("casing не важен", () => {
    expect(parseGoalCommand("GOAL: сделать X")).toEqual({ kind: "goal", text: "сделать X" })
  })
})

describe("parseVerdict", () => {
  test("все три вердикта", () => {
    expect(parseVerdict("GOAL_DONE: файл создан")).toEqual({
      kind: "done",
      detail: "файл создан",
    })
    expect(parseVerdict("GOAL_NOT_DONE: нет второго поста")).toEqual({
      kind: "not_done",
      detail: "нет второго поста",
    })
    expect(parseVerdict("GOAL_IMPOSSIBLE: цель противоречит репо")).toEqual({
      kind: "impossible",
      detail: "цель противоречит репо",
    })
  })

  test("casing и лишние пробелы", () => {
    expect(parseVerdict("  goal_done:  всё ок ")).toEqual({ kind: "done", detail: "всё ок" })
  })

  test("неразбираемый ответ — fail-safe NOT_DONE", () => {
    expect(parseVerdict("агент пересказал что-то невнятное")).toEqual({
      kind: "not_done",
      detail: "агент пересказал что-то невнятное",
    })
    expect(parseVerdict("")).toEqual({ kind: "not_done", detail: "" })
  })

  test("вердикт не в начале строки — fail-safe", () => {
    expect(parseVerdict("Кажется, GOAL_DONE!")).toEqual({
      kind: "not_done",
      detail: "Кажется, GOAL_DONE!",
    })
  })
})

describe("renderTranscript", () => {
  const msgs: TranscriptMessage[] = [
    { type: "user", time: { created: 100 }, content: [{ type: "text", text: "цель" }] },
    { type: "assistant", time: { created: 200 }, content: [{ type: "text", text: "работаю" }] },
    { type: "tool", time: { created: 250 }, content: [{ type: "text", text: "мусор тула" }] },
    { type: "assistant", time: { created: 300 }, content: [{ type: "text", text: "готово" }] },
    { type: "user", time: { created: 50 }, content: [{ type: "text", text: "до цели" }] },
  ]

  test("фильтрует по since и ролям, маркирует автора", () => {
    expect(renderTranscript(msgs, 90, 10_000)).toBe("USER: цель\n\nAGENT: работаю\n\nAGENT: готово")
  })

  test("since отсекает старые сообщения", () => {
    expect(renderTranscript(msgs, 150, 10_000)).toBe("AGENT: работаю\n\nAGENT: готово")
  })

  test("cap обрезает с хвоста (свежее важнее)", () => {
    const out = renderTranscript(msgs, 90, 20)
    expect(out.length).toBeLessThanOrEqual(20)
    expect(out).toContain("готово") // свежий финал сохранился
  })

  test("пустой список", () => {
    expect(renderTranscript([], 0, 1000)).toBe("")
  })

  test("text-части без текста не ломают рендер", () => {
    const weird: TranscriptMessage[] = [
      { type: "assistant", time: { created: 100 }, content: [{ type: "text" }, { type: "tool" }] },
    ]
    expect(renderTranscript(weird, 0, 1000)).toBe("")
  })
})

describe("judgePrompt", () => {
  test("содержит цель, транскрипт и все три вердикта", () => {
    const p = judgePrompt("сделай X", "USER: сделай X")
    expect(p).toContain("ЦЕЛЬ: сделай X")
    expect(p).toContain("USER: сделай X")
    expect(p).toContain("GOAL_DONE:")
    expect(p).toContain("GOAL_NOT_DONE:")
    expect(p).toContain("GOAL_IMPOSSIBLE:")
  })

  test("пустой транскрипт заменяется заглушкой", () => {
    expect(judgePrompt("X", "")).toContain("(пусто)")
  })
})

describe("lastMessageText", () => {
  test("берёт последнее сообщение, ординалы по порядку", () => {
    const segs = new Map([
      ["aaa:0", "нереция"],
      ["bbb:0", "финал A"],
      ["bbb:1", "финал B"],
    ])
    expect(lastMessageText(segs)).toBe("финал A\nфинал B")
  })

  test("err-фолбэк без регулярных сегментов", () => {
    expect(lastMessageText(new Map([["err", "⚠️ упал"]]))).toBe("⚠️ упал")
  })

  test("пустой", () => {
    expect(lastMessageText(new Map())).toBe("")
  })
})

describe("lastAssistantText", () => {
  test("последний assistant с текстом", () => {
    const msgs: TranscriptMessage[] = [
      { type: "user", content: [{ type: "text", text: "вопрос" }] },
      { type: "assistant", content: [{ type: "text", text: "ответ" }] },
    ]
    expect(lastAssistantText(msgs)).toBe("ответ")
  })

  test("assistant без text-частей пропускается", () => {
    const msgs: TranscriptMessage[] = [
      { type: "assistant", content: [{ type: "tool" }] },
      { type: "assistant", content: [{ type: "text", text: "настоящий" }] },
    ]
    expect(lastAssistantText(msgs)).toBe("настоящий")
  })

  test("пусто — пустая строка", () => {
    expect(lastAssistantText([])).toBe("")
  })
})
