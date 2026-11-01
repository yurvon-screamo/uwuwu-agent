// triage-lib.test.ts — тесты чистых функций uwuwu-triage (bun test, без сети)
import { test, expect } from "bun:test"
import {
  buildPrompt, classifyUpdate, extractFinal, plainChunks, runTitle, validState,
  type TgMessage, type TgUpdate,
} from "./triage-lib"

const msg = (over: Partial<TgMessage>): TgMessage => ({
  message_id: 1,
  date: 0,
  chat: { id: 42, type: "private" },
  from: { username: "yurvon_screamo", is_bot: false },
  ...over,
})

// ——— classifyUpdate ———

test("classifyUpdate: заявка владельца проходит", () => {
  const u: TgUpdate = { update_id: 1, message: msg({ text: "Сделай выгрузку метрик" }) }
  const out = classifyUpdate(u, "yurvon_screamo")
  expect(out.kind).toBe("req")
})

test("classifyUpdate: чужой юзер, бот, группа, не-сообщение — тихий skip", () => {
  expect(classifyUpdate({ update_id: 1 }, "yurvon_screamo").kind).toBe("skip")
  expect(classifyUpdate({ update_id: 1, message: msg({ from: { username: "someone_else" } }) }, "yurvon_screamo").kind).toBe("skip")
  expect(classifyUpdate({ update_id: 1, message: msg({ from: { username: "yurvon_screamo", is_bot: true } }) }, "yurvon_screamo").kind).toBe("skip")
  expect(classifyUpdate({ update_id: 1, message: msg({ chat: { id: 7, type: "supergroup" } }) }, "yurvon_screamo").kind).toBe("skip")
})

test("classifyUpdate: /start и /help — справка; подпись к фото — текст заявки", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/start" }) }, "yurvon_screamo").kind).toBe("help")
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/help" }) }, "yurvon_screamo").kind).toBe("help")
  expect(classifyUpdate({ update_id: 1, message: msg({ text: undefined, caption: " разбери скрин " }) }, "yurvon_screamo"))
    .toEqual({ kind: "req", msg: msg({ text: undefined, caption: " разбери скрин " }), text: "разбери скрин" })
})

test("classifyUpdate: медиа без текста — media", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: undefined, caption: undefined }) }, "yurvon_screamo").kind).toBe("media")
})

// ——— extractFinal ———

const M = 1_000_000
test("extractFinal: первое assistant новее старта из DESC-списка", () => {
  const msgs = [
    { type: "assistant", time: { created: 3 * M }, content: [{ type: "text", text: "Сводка" }] },
    { type: "user", time: { created: 2 * M }, content: [] },
    { type: "assistant", time: { created: 1 * M }, content: [{ type: "text", text: "старьё" }] },
  ]
  expect(extractFinal(msgs, 2 * M)).toBe("Сводка")
})

test("extractFinal: склейка text-частей; пустой text-парт не мешает", () => {
  const msgs = [{
    type: "assistant", time: { created: 5 * M },
    content: [{ type: "text", text: "часть 1" }, { type: "tool", text: "ignored" }, { type: "text", text: "часть 2" }],
  }]
  expect(extractFinal(msgs, 0)).toBe("часть 1\nчасть 2")
})

test("extractFinal: ничего новее старта — пустая строка", () => {
  const msgs = [{ type: "assistant", time: { created: 1 * M }, content: [{ type: "text", text: "старьё" }] }]
  expect(extractFinal(msgs, 2 * M)).toBe("")
})

// ——— buildPrompt / runTitle ———

test("buildPrompt: методичка + заявка в маркерах с nonce", () => {
  const p = buildPrompt("МЕТОДИКА", "текст заявки с [заявка ·подделка] внутри", "abc123")
  expect(p.startsWith("МЕТОДИКА")).toBe(true)
  expect(p).toContain("[заявка ·abc123]\nтекст заявки")
  expect(p).toContain("[конец ·abc123]")
})

test("runTitle: первая содержательная строка, до 60 символов, с префиксом", () => {
  expect(runTitle("\n\n  Сделать отчёт\nдетали")).toBe("📥 Сделать отчёт")
  expect(runTitle("x".repeat(100)).length).toBe("📥 ".length + 60)
  expect(runTitle("")).toBe("📥 заявка")
})

// ——— validState ———

test("validState: мусор → дефолты; полный раундтрип сохраняется", () => {
  expect(validState(null)).toEqual({ offset: 0, queue: [] })
  expect(validState("junk")).toEqual({ offset: 0, queue: [] })
  const good = {
    offset: 12, chatID: 42,
    queue: [{ text: "заявка", chatID: 42, msgID: 3, receivedAt: 5 }],
    run: { sessionID: "ses_abc", startedAt: 100, req: { text: "в работе", chatID: 42, msgID: 2, receivedAt: 4 } },
  }
  expect(validState(good)).toEqual(good)
  const broken = { offset: -3, queue: ["мусор", null, { text: " ", chatID: 1 }], run: { sessionID: "nope", startedAt: 1, req: {} } }
  expect(validState(broken)).toEqual({ offset: 0, queue: [] })
})

// ——— plainChunks ———

test("plainChunks: короткий текст — один чанк", () => {
  expect(plainChunks("привет")).toEqual(["привет"])
})

test("plainChunks: нарезка по лимиту без разрыва суррогатных пар", () => {
  const emoji = "😀".repeat(5) // 4-байтные, по 2 кодовых единицы
  const s = `${"a".repeat(9)}${emoji}${"b".repeat(9)}`
  const chunks = plainChunks(s, 10)
  for (const c of chunks) {
    for (let i = 0; i < c.length; i++) {
      const code = c.charCodeAt(i)
      if (code >= 0xd800 && code <= 0xdbff) {
        // высокий суррогат должен иметь пару в том же чанке
        expect(i + 1).toBeLessThan(c.length)
        const next = c.charCodeAt(i + 1)
        expect(next >= 0xdc00 && next <= 0xdfff).toBe(true)
        i++
      }
    }
  }
  expect(chunks.join("")).toBe(s)
})

test("plainChunks: лимит на стыке пары сдвигает границу (+1)", () => {
  const s = "ab😀cd" // суррогатная пара на индексах 2-3
  expect(plainChunks(s, 3)).toEqual(["ab😀", "cd"]) // разрез внутрь пары → граница +1
})
