// triage-lib.test.ts — тесты чистых функций uwuwu-triage (bun test, без сети)
import { test, expect } from "bun:test"
import {
  alignChunks, batchReadyAt, classifyUpdate, dropBatch, extractFinal, joinBatch, plainChunks,
  promptLandedAt, takeBatch, validState,
  type Request, type SessionMsg, type TgUpdate,
} from "./triage-lib"

const msg = (over: Partial<NonNullable<TgUpdate["message"]>>): NonNullable<TgUpdate["message"]> => ({
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

test("classifyUpdate: /reset — пересоздание сессии", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/reset" }) }, "yurvon_screamo").kind).toBe("reset")
})

test("classifyUpdate: медиа без текста — media", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: undefined, caption: undefined }) }, "yurvon_screamo").kind).toBe("media")
})

test("classifyUpdate: фото/документ — заявка даже без текста", () => {
  const photo = msg({ text: undefined, caption: undefined, photo: [{ file_id: "f1" }, { file_id: "f2" }] })
  const out = classifyUpdate({ update_id: 1, message: photo }, "yurvon_screamo")
  expect(out.kind).toBe("req")
  if (out.kind === "req") expect(out.text).toBe("")

  const doc = msg({ text: undefined, caption: "логи", document: { file_id: "d1", file_name: "app.log" } })
  const out2 = classifyUpdate({ update_id: 2, message: doc }, "yurvon_screamo")
  expect(out2.kind).toBe("req")
  if (out2.kind === "req") expect(out2.text).toBe("логи")
})

test("classifyUpdate: edited_message — edit с новым текстом", () => {
  const e = msg({ message_id: 7, text: "поправленный текст" })
  const out = classifyUpdate({ update_id: 1, edited_message: e }, "yurvon_screamo")
  expect(out).toEqual({ kind: "edit", msg: e, text: "поправленный текст" })
  // чужой редact — тихо
  expect(classifyUpdate({ update_id: 1, edited_message: msg({ text: "x" }) }, "not_owner").kind).toBe("skip")
})

// ——— extractFinal / promptLandedAt ———

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

test("promptLandedAt: свой промпт находится по тексту; чужой (double-text ≤2с) не ловится", () => {
  const T = 10 * M
  const msgs: SessionMsg[] = [
    { type: "assistant", time: { created: 30 * M }, content: [] },
    { type: "user", time: { created: 9 * M }, content: [{ type: "text", text: "предыдущий вопрос" }] }, // промпт прошлого хода в окне
    { type: "user", time: { created: 10 * M }, content: [{ type: "text", text: "сделай отчёт" }] },     // наш промпт
  ]
  expect(promptLandedAt(msgs, 8 * M, "сделай отчёт")).toBe(T)
  // тот же текст, границы нет — только наш ход матчится
  expect(promptLandedAt(msgs, 20 * M, "сделай отчёт")).toBe(null)
  // идентичный текст дважды (ре-дубль юзера) — берём самый ранний в окне: benign
  const dup: SessionMsg[] = [
    { type: "user", time: { created: 10 * M }, content: [{ type: "text", text: "ок" }] },
    { type: "user", time: { created: 11 * M }, content: [{ type: "text", text: "ок" }] },
  ]
  expect(promptLandedAt(dup, 8 * M, "ок")).toBe(10 * M)
})

test("promptLandedAt: длинный промпц матчится по префиксу (пережат на части)", () => {
  const long = "x".repeat(300)
  const msgs: SessionMsg[] = [
    { type: "user", time: { created: 5 * M }, content: [{ type: "text", text: long.slice(0, 200) }] },
    { type: "user", time: { created: 6 * M }, content: [{ type: "text", text: long.slice(200) }] },
  ]
  expect(promptLandedAt(msgs, 0, long)).toBe(5 * M)
})

// ——— validState ———

test("validState: мусор → дефолты; полный раундтрип сохраняется", () => {
  expect(validState(null)).toEqual({ offset: 0, queue: [] })
  expect(validState("junk")).toEqual({ offset: 0, queue: [] })
  expect(validState({ offset: 1.5 })).toEqual({ offset: 0, queue: [] }) // нецелый offset — невалиден
  const good = {
    offset: 12, chatID: 42, session: "ses_dialog", broken: 1, lastResetAt: 5, pausedUntil: 9,
    queue: [{ text: "заявка", chatID: 42, msgID: 3, receivedAt: 5, attempts: 2 }],
    run: { sessionID: "ses_dialog", startedAt: 100, req: { text: "в работе", chatID: 42, msgID: 2, receivedAt: 4 } },
  }
  expect(validState(good)).toEqual(good)
  const broken = {
    offset: -3, session: "nope", broken: -1, lastResetAt: -5,
    queue: ["мусор", null, { text: " ", chatID: 1 }, { text: "ок", chatID: 1, msgID: 2, attempts: -1 }],
    run: { sessionID: "nope", startedAt: 1, req: {} },
  }
  expect(validState(broken)).toEqual({ offset: 0, queue: [] })
  // окно отправки: run с startedAt=0 (промпт не подтверждён) — валиден и персистится
  expect(validState({ run: { sessionID: "ses_x", startedAt: 0, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 } } }).run?.startedAt).toBe(0)
})

// ——— батчинг ———

const R = (over: Partial<Request>): Request => ({
  text: "t", chatID: 1, msgID: 1, receivedAt: 0, ...over,
})

test("batchReadyAt: тишина DELAY после последнего; кап MAX от самого старого", () => {
  const delay = 60_000, max = 300_000
  const q = [R({ msgID: 1, receivedAt: 1000 })]
  expect(batchReadyAt(q, 1000, delay, max)).toBe(61_000)          // тишина с последнего push
  expect(batchReadyAt(q, 5000, delay, max)).toBe(65_000)          // push позже — окно сдвигается
  // бесконечный поток: тишина никогда — но кап отпускает самое старое
  const stream = [R({ msgID: 1, receivedAt: 1000 }), R({ msgID: 2, receivedAt: 200_000 })]
  expect(batchReadyAt(stream, 200_000, delay, max)).toBe(260_000) // min(тишина 260_000, кап 301_000)
  expect(batchReadyAt([], 0, delay, max)).toBe(null)
})

test("takeBatch/joinBatch/dropBatch: батч = всё до readyAt, склейка и удаление юнитом", () => {
  const q = [
    R({ msgID: 1, receivedAt: 1000, text: "первое" }),
    R({ msgID: 2, receivedAt: 2000, text: "второе", attachments: [{ path: "/m/2.jpg", name: "2.jpg" }] }),
    R({ msgID: 3, receivedAt: 9000, text: "опоздавшее" }),
  ]
  const batch = takeBatch(q, 5000)
  expect(batch.map((r) => r.msgID)).toEqual([1, 2])
  const joined = joinBatch(batch)
  expect(joined.text).toBe("первое\n\nвторое")
  expect(joined.msgIDs).toEqual([1, 2])
  expect(joined.attachments).toEqual([{ path: "/m/2.jpg", name: "2.jpg" }])
  const rest = dropBatch(q, joined)
  expect(rest.map((r) => r.msgID)).toEqual([3]) // опоздавшее — следующий батч
})

test("joinBatch: одиночное сообщение не получает msgIDs/attachments", () => {
  const j = joinBatch([R({ text: "соло" })])
  expect(j.msgIDs).toBeUndefined()
  expect(j.attachments).toBeUndefined()
})

test("joinBatch: вложения батча склеиваются (кап 10)", () => {
  const items = Array.from({ length: 12 }, (_, i) =>
    R({ msgID: i + 1, receivedAt: i, attachments: [{ path: `/m/${i}`, name: `${i}.jpg` }] }))
  const j = joinBatch(items)
  expect(j.attachments?.length).toBe(10)
})

// ——— plainChunks / alignChunks ———

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

test("alignChunks: куски покрывают исходник без потерь и дублей", () => {
  const src = "aaaa\n\nbbbb **жирный** cccc".repeat(30)
  const parts = ["<b>x</b>".repeat(40), "plain".repeat(60), "<i>y</i>".repeat(20)]
  const aligned = alignChunks(parts, src)
  expect(aligned.join("")).toBe(src)
})

test("alignChunks: границы не рвут суррогатные пары исходника", () => {
  const src = `привет ${"😀".repeat(200)} пока`
  const aligned = alignChunks(["ab", "cd", "ef", "gh", "ij"], src)
  for (const c of aligned) {
    for (let i = 0; i < c.length; i++) {
      const code = c.charCodeAt(i)
      if (code >= 0xd800 && code <= 0xdbff) {
        expect(i + 1).toBeLessThan(c.length)
        const next = c.charCodeAt(i + 1)
        expect(next >= 0xdc00 && next <= 0xdfff).toBe(true)
        i++
      }
    }
  }
})

test("alignChunks: пустой исходник — пустые куски по числу частей", () => {
  expect(alignChunks(["a", "b"], "")).toEqual(["", ""])
})
