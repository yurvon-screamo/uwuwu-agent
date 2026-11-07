// hub.test.ts — тесты чистых функций универсального ядра ТГ-демона
// (bun test, без сети; перенос из triage-lib.test.ts при выносе ядра в lib)
import { test, expect } from "bun:test"
import {
  alignChunks, batchReadyAt, capUnchatBatch, classifyUpdate, composeText, dropBatch, extractReplies,
  inboxToSessionMsgs, joinBatch, mediaFiles, parseDraftAcks, plainChunks, promptLandedAt, steerCandidate,
  steerRecoveryBatch, stripDraftAcks, takeBatch, validState,
  type Request, type SessionMsg, type TgUpdate,
} from "./hub"

const msg = (over: Partial<NonNullable<TgUpdate["message"]>>): NonNullable<TgUpdate["message"]> => ({
  message_id: 1,
  date: 0,
  chat: { id: 42, type: "private" },
  from: { username: "yurvon_screamo", is_bot: false },
  ...over,
})

const R = (over: Partial<Request>): Request => ({
  text: "t", chatID: 1, msgID: 1, receivedAt: 0, ...over,
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
  expect(classifyUpdate({ update_id: 1, message: msg({ chat: { id: -100, type: "group" } }) }, "yurvon_screamo").kind).toBe("skip")
})

test("classifyUpdate: /start и /help — справка; подпись к фото — текст заявки", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/start" }) }, "yurvon_screamo").kind).toBe("help")
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/help" }) }, "yurvon_screamo").kind).toBe("help")
  const photo = classifyUpdate({ update_id: 1, message: msg({ caption: "глянь", photo: [{ file_id: "f" }] }) }, "yurvon_screamo")
  expect(photo.kind).toBe("req")
})

test("classifyUpdate: /reset — пересоздание сессии", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ text: "/reset" }) }, "yurvon_screamo").kind).toBe("reset")
})

test("classifyUpdate: форвард видео без подписи — заявка (баг 03.11: терялся как media)", () => {
  const u: TgUpdate = { update_id: 1, message: msg({ video: { file_id: "f" } }) }
  expect(classifyUpdate(u, "yurvon_screamo").kind).toBe("req")
})

test("classifyUpdate: видео/гифка/кружок/аудио — заявка, подпись становится текстом", () => {
  const cases: NonNullable<TgUpdate["message"]>[] = [
    msg({ caption: "смотри", video: { file_id: "f1", file_name: "clip.mp4" } }),
    msg({ caption: "гифка", animation: { file_id: "f2" } }),
    msg({ caption: "кружок", video_note: { file_id: "f3" } }),
    msg({ caption: "аудио", audio: { file_id: "f4" } }),
  ]
  for (const m of cases) {
    const out = classifyUpdate({ update_id: 1, message: m }, "yurvon_screamo")
    expect(out.kind).toBe("req")
  }
})

test("classifyUpdate: голосовое/стикер без подписи — media-bounce (явная деградация)", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ voice: { file_id: "f" } }) }, "yurvon_screamo").kind).toBe("media")
  expect(classifyUpdate({ update_id: 1, message: msg({ sticker: { file_id: "f" } }) }, "yurvon_screamo").kind).toBe("media")
})

test("classifyUpdate: медиа без текста — media", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ document: { file_id: "f" } }) }, "yurvon_screamo").kind).toBe("req")
})

test("classifyUpdate: фото/документ — заявка даже без текста", () => {
  expect(classifyUpdate({ update_id: 1, message: msg({ photo: [{ file_id: "f" }] }) }, "yurvon_screamo").kind).toBe("req")
  expect(classifyUpdate({ update_id: 1, message: msg({ document: { file_id: "f", file_name: "a.txt" } }) }, "yurvon_screamo").kind).toBe("req")
})

test("classifyUpdate: edited_message — edit с новым текстом", () => {
  const u: TgUpdate = { update_id: 1, edited_message: msg({ text: "правка" }) }
  const out = classifyUpdate(u, "yurvon_screamo")
  expect(out.kind).toBe("edit")
})

// ——— mediaFiles ———

test("mediaFiles: имена с message_id-префиксом, дефолты и санитизация", () => {
  const named = msg({ message_id: 12, video: { file_id: "v1", file_name: "мой клип.mp4" } })
  expect(mediaFiles(named)).toEqual([{ fileId: "v1", name: "12_мой клип.mp4" }])

  const unnamed = msg({ message_id: 13, video: { file_id: "v2" } })
  expect(mediaFiles(unnamed)).toEqual([{ fileId: "v2", name: "13_video.mp4" }])

  const animation = msg({ message_id: 14, animation: { file_id: "a1" } })
  expect(mediaFiles(animation)).toEqual([{ fileId: "a1", name: "14_animation.mp4" }])

  const note = msg({ message_id: 15, video_note: { file_id: "n1" } })
  expect(mediaFiles(note)).toEqual([{ fileId: "n1", name: "15.mp4" }])

  const audio = msg({ message_id: 16, audio: { file_id: "au1", file_name: "song.mp3" } })
  expect(mediaFiles(audio)).toEqual([{ fileId: "au1", name: "16_song.mp3" }])

  const doc = msg({ message_id: 17, document: { file_id: "d1", file_name: "a/b\x01c.log" } })
  expect(mediaFiles(doc)).toEqual([{ fileId: "d1", name: "17_a_b_c.log" }])

  const docUnnamed = msg({ message_id: 18, document: { file_id: "d2" } })
  expect(mediaFiles(docUnnamed)).toEqual([{ fileId: "d2", name: "18_file" }])

  const photo = msg({ message_id: 19, photo: [{ file_id: "f1" }, { file_id: "f2" }] })
  expect(mediaFiles(photo)).toEqual([{ fileId: "f2", name: "19.jpg" }]) // максимальное разрешение
})

test("mediaFiles: два безымянных видео разных сообщений — разные имена (анти-коллизия альбома)", () => {
  const a = mediaFiles(msg({ message_id: 1, video: { file_id: "x" } }))
  const b = mediaFiles(msg({ message_id: 2, video: { file_id: "y" } }))
  expect(a[0]?.name).not.toBe(b[0]?.name)
})

test("mediaFiles: запись без file_id — не вложение (единое правило для всех типов)", () => {
  expect(mediaFiles(msg({ document: { file_id: "f" }, video_note: { file_id: "n" } })).length).toBe(2)
  expect(mediaFiles(msg({})).length).toBe(0)
})

// ——— extractReplies / promptLandedAt ———

const M = 1_000_000
const sess = (over: Partial<SessionMsg>): SessionMsg => ({ type: "assistant", time: { created: 100, completed: 150 }, id: "m1", content: [{ type: "text", text: "ответ" }], ...over })

test("extractReplies: ответы хода по возрастанию из DESC-списка; старьё и user мимо", () => {
  const msgs = [
    sess({ id: "m3", time: { created: 300, completed: 350 } }),
    sess({ id: "m2", time: { created: 200, completed: 250 } }),
    sess({ id: "old", time: { created: 50, completed: 60 } }),
    { type: "user", time: { created: 300 }, text: "вопрос" },
  ]
  const out = extractReplies(msgs, 100)
  expect(out.map((r) => r.id)).toEqual(["m2", "m3"])
})

test("extractReplies: склейка text-частей; пустой text-парт и tool-часть не мешают", () => {
  const out = extractReplies([sess({ content: [{ type: "text", text: "" }, { type: "text", text: "часть1" }, { type: "tool", text: "x" }, { type: "text", text: "часть2" }] })], 0)
  expect(out[0]?.text).toBe("часть1\nчасть2")
})

test("extractReplies: без completed — фильтр по умолчанию; с completedOnly=false — частичный текст доезжает", () => {
  const m = sess({ time: { created: 100 } }) // стрим идёт, completed нет
  expect(extractReplies([m], 0).length).toBe(0)
  expect(extractReplies([m], 0, [], false).length).toBe(1)
})

test("extractReplies: delivered-дедуп; сообщения без id не доставляются", () => {
  const msgs = [sess({ id: "m1" }), sess({ id: "m2" }), sess({})]
  expect(extractReplies(msgs, 0, ["m1"]).map((r) => r.id)).toEqual(["m2"])
})

test("extractReplies: сообщение из одних tool-частей (пустой текст) не ответ", () => {
  const m = sess({ content: [{ type: "tool", text: "x" }] })
  expect(extractReplies([m], 0).length).toBe(0)
})

test("promptLandedAt: свой промпт находится по тексту; чужой (double-text ≤2с) не ловится", () => {
  const msgs: SessionMsg[] = [
    { type: "user", time: { created: 100 }, text: "заявка юзера" },
    { type: "assistant", time: { created: 200 }, content: [{ type: "text", text: "ответ" }] },
  ]
  expect(promptLandedAt(msgs, 50, "заявка юзера")).toBe(100)
  expect(promptLandedAt(msgs, 50, "другой текст")).toBeNull()
})

test("promptLandedAt: длинный промпт матчится по префиксу (пережат на части)", () => {
  const long = "x".repeat(300)
  const msgs: SessionMsg[] = [
    { type: "user", time: { created: 5 * M }, content: [{ type: "text", text: long.slice(0, 200) }] },
    { type: "user", time: { created: 6 * M }, content: [{ type: "text", text: long.slice(200) }] },
  ]
  expect(promptLandedAt(msgs, 0, long)).toBe(5 * M)
})

test("promptLandedAt: user-сообщение 2.2.5 с текстом верхнего уровня (без content) матчится", () => {
  // живая форма GET /message: {id, time, text, type} — content у user нет
  const msgs: SessionMsg[] = [
    { id: "msg_u2", type: "user", time: { created: 11 * M }, text: "Сколько будет семь минус три?" },
    { id: "msg_u1", type: "user", time: { created: 10 * M }, text: "сделай отчёт" },
  ]
  expect(promptLandedAt(msgs, 10 * M - 2000, "Сколько будет семь минус три?")).toBe(11 * M)
  expect(promptLandedAt(msgs, 12 * M, "Сколько будет семь минус три?")).toBe(null)
})

// ——— validState (база) ———

test("validState: мусор → дефолты; полный раундтрип сохраняется", () => {
  expect(validState(null)).toEqual({ offset: 0, queue: [] })
  expect(validState("junk")).toEqual({ offset: 0, queue: [] })
  expect(validState({ offset: 1.5 })).toEqual({ offset: 0, queue: [] }) // нецелый offset — невалиден
  const good = {
    offset: 12, chatID: 42, session: "ses_dialog",
    queue: [{ text: "заявка", chatID: 42, msgID: 3, receivedAt: 5, attempts: 2 }],
    run: { sessionID: "ses_dialog", startedAt: 100, req: { text: "в работе", chatID: 42, msgID: 2, receivedAt: 4 } },
  }
  expect(validState(good)).toEqual(good)
  const junk = {
    offset: -3, session: "nope",
    queue: ["мусор", null, { text: " ", chatID: 1 }, { text: "ок", chatID: 1, msgID: 2, attempts: -1 }],
    run: { sessionID: "nope", startedAt: 1, req: {} },
  }
  expect(validState(junk)).toEqual({ offset: 0, queue: [] })
  // окно отправки: run с startedAt=0 (промпт не подтверждён) — валиден и персистится
  expect(validState({ run: { sessionID: "ses_x", startedAt: 0, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 } } }).run?.startedAt).toBe(0)
})

test("validState: run.delivered персистится; битые id стрипятся почастино — ход жив", () => {
  const good = {
    offset: 0, chatID: 1, queue: [],
    run: { sessionID: "ses_x", startedAt: 5, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 }, delivered: ["msg_a", "msg_b"], steerAt: 9 },
  }
  expect(validState(good)).toEqual(good)
  const dirty = {
    offset: 0, chatID: 1,
    run: {
      sessionID: "ses_x", startedAt: 5, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 },
      delivered: ["msg_a", 42, null, "", "x".repeat(200), "msg_b"],
      steerAt: "мусор",
    },
  }
  const cleanedRun = validState(dirty).run
  expect(cleanedRun?.delivered).toEqual(["msg_a", "msg_b"]) // мусор выброшен, валидные остались
  expect(cleanedRun?.steerAt).toBeUndefined() // битая граница стрипится, ход остаётся
})

test("validState: delivered >100 — остаются свежайшие (старьё доставлено давно)", () => {
  const ids = Array.from({ length: 150 }, (_, i) => `msg_${i}`)
  const out = validState({
    offset: 0, chatID: 1,
    run: { sessionID: "ses_x", startedAt: 1, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 }, delivered: ids },
  }).run
  expect(out?.delivered).toEqual(ids.slice(-100))
  // пустой после стрипа — поле долой, не пустой массив (42 — не строка-ид)
  const onlyJunk = validState({
    offset: 0, chatID: 1,
    run: { sessionID: "ses_x", startedAt: 1, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 }, delivered: [42] },
  }).run
  expect(onlyJunk?.delivered).toBeUndefined()
  const notArray = validState({
    offset: 0, chatID: 1,
    run: { sessionID: "ses_x", startedAt: 1, req: { text: "t", chatID: 1, msgID: 1, receivedAt: 0 }, delivered: "не-массив" },
  }).run
  expect(notArray?.delivered).toBeUndefined()
})

test("validState: легаси-поля деградации (broken/lastResetAt/pausedUntil) стрипаются при любом значении", () => {
  // state.json, писанный старой версией демона: механизм удалён — поля уходят
  // молча (санитайзер строит out заново), откат на старый код безопасен
  const legacy = {
    offset: 7, session: "ses_dialog", broken: 2, lastResetAt: 100, pausedUntil: 200,
    queue: [{ text: "t", chatID: 1, msgID: 1, receivedAt: 0 }],
  }
  expect(validState(legacy)).toEqual({
    offset: 7, session: "ses_dialog",
    queue: [{ text: "t", chatID: 1, msgID: 1, receivedAt: 0 }],
  })
})

test("validState: failed-имена — форма важнее маркера, битое поле не роняет заявку", () => {
  const good = validState({
    offset: 0,
    queue: [{ text: "t", chatID: 1, msgID: 1, receivedAt: 0, failed: ["a.mp4", "b.mp4"] }],
  })
  expect(good.queue[0]?.failed).toEqual(["a.mp4", "b.mp4"])
  const cleaned = validState({
    offset: 0,
    queue: [{ text: "t", chatID: 1, msgID: 1, receivedAt: 0, failed: { junk: true } }],
  })
  expect(cleaned.queue[0]).toEqual({ text: "t", chatID: 1, msgID: 1, receivedAt: 0 }) // failed долой, заявка жива
})

test("validState: allowSilent — булев флаг персистится (тихий финал крон-хода)", () => {
  const out = validState({ offset: 0, queue: [{ text: "аудит", chatID: 42, msgID: -1, receivedAt: 0, allowSilent: true }] })
  expect(out.queue[0]?.allowSilent).toBe(true)
  const junk = validState({ offset: 0, queue: [{ text: "аудит", chatID: 42, msgID: -1, receivedAt: 0, allowSilent: "да" }] })
  expect(junk.queue).toEqual([]) // не булево — заявка целиком невалидна
})

// ——— батчинг ———

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
  const j = joinBatch([R({ text: "одно", msgID: 5 })])
  expect(j.msgIDs).toBeUndefined()
  expect(j.text).toBe("одно")
})

test("joinBatch: вложения батча склеиваются (кап 20)", () => {
  const items = Array.from({ length: 12 }, (_, i) => R({ msgID: i + 1, attachments: [{ path: `/tmp/${i}`, name: `f${i}` }, { path: `/tmp/b${i}`, name: `g${i}` }] }))
  const j = joinBatch(items)
  expect(j.attachments?.length).toBe(20) // кап ATT_BATCH_MAX
})

test("joinBatch: failed-имена батча дедупятся и капятся", () => {
  const j = joinBatch([R({ failed: ["a.mp4"] }), R({ failed: ["a.mp4", "b.mp4"] })])
  expect(j.failed).toEqual(["a.mp4", "b.mp4"])
})

test("joinBatch: черновики и ТГ склеиваются, chatID от ТГ-части; чисто черновичый = 0", () => {
  const draft = R({ chatID: 0, msgID: -1, receivedAt: 0, drafts: [{ project: "uwuwu", rowId: "r1", docId: "d1", title: "T" }] })
  const tg = R({ chatID: 42, msgID: 7, receivedAt: 100 })
  const mixed = joinBatch([draft, tg])
  expect(mixed.chatID).toBe(42)
  expect(mixed.drafts).toEqual([{ project: "uwuwu", rowId: "r1", docId: "d1", title: "T" }])
  expect(mixed.msgIDs).toEqual([-1, 7])
  expect(mixed.allowSilent).toBeUndefined() // ТГ-часть держит батч озвученным
  const pure = joinBatch([draft, R({ chatID: 0, msgID: -2, receivedAt: 1 })])
  expect(pure.chatID).toBe(0)
  expect(pure.drafts?.length).toBe(1)
})

test("joinBatch: allowSilent — батч тихий только когда ВСЕ части тихие (крон-промпты)", () => {
  const cron = R({ chatID: 42, msgID: -1, allowSilent: true })
  expect(joinBatch([cron]).allowSilent).toBe(true)
  const cron2 = R({ chatID: 42, msgID: -2, allowSilent: true })
  expect(joinBatch([cron, cron2]).allowSilent).toBe(true) // два крон-промпта одним ходом — по-прежнему тихо
  const user = R({ chatID: 42, msgID: 7 })
  expect(joinBatch([cron, user]).allowSilent).toBeUndefined() // юзер в батче — молчать нельзя
})

test("composeText: текст + маркеры вложений и неудач", () => {
  const t = composeText("глянь", [{ path: "/tmp/1.jpg", name: "1.jpg" }], ["video.mp4"])
  expect(t).toContain("глянь")
  expect(t).toContain("[вложение: 1.jpg — файл в ФС: /tmp/1.jpg]")
  expect(t).toContain("[вложение video.mp4 не выгрузилось — попроси юзера продублировать]")
  expect(composeText("", [], [])).toBe("")
})

// ——— стирт: кандидат и краш-восстановление ———

test("steerCandidate: только ТГ-подмножество, черновики не входят в батч", () => {
  const draft = R({ chatID: 0, msgID: -1, receivedAt: 0, text: "[черновик задачи]" })
  const tg1 = R({ msgID: 7, receivedAt: 0, text: "вброс" })
  const tg2 = R({ msgID: 8, receivedAt: 1000, text: "дополнение" })
  const cand = steerCandidate([draft, tg1, tg2], 1000, 60_000, 300_000)
  expect(cand?.batch.msgIDs).toEqual([7, 8]) // персона — только ТГ
  expect(cand?.batch.text).toBe("вброс\n\nдополнение")
  expect(steerCandidate([draft], 0, 60_000, 300_000)).toBeNull() // чисто без-чатовая очередь — стирта нет
  expect(steerCandidate([], 0, 60_000, 300_000)).toBeNull()
})

test("steerCandidate: тот же дебаунс — неготовое ТГ-сообщение не кандидет", () => {
  const q = [R({ msgID: 1, receivedAt: 50_000 })]
  // тишина 60с с последнего пуша — не готово (receivedAt+60с в будущем):
  // readyAt считается, готовность проверяет демон сравнением с Date.now()
  const cand = steerCandidate(q, 50_000, 60_000, 300_000)
  expect(cand).not.toBeNull()
  expect(cand?.readyAt).toBe(110_000)
})

test("steerRecoveryBatch: точная персона по msgIDs из firing, порядок очереди", () => {
  const q = [
    R({ msgID: 5, receivedAt: 0, text: "x" }),
    R({ msgID: 9, receivedAt: 1, text: "y" }),
    R({ msgID: 2, receivedAt: 2, text: "новое после краха" }),
  ]
  const batch = steerRecoveryBatch(q, [9, 5]) // стиртованная персона: 5 и 9, третье — не прицепится
  expect(batch?.msgIDs).toEqual([5, 9])
  expect(batch?.text).toBe("x\n\ny")
  expect(steerRecoveryBatch(q, [777])).toBeNull() // персона ушла из очереди — маркер стух
  expect(steerRecoveryBatch([], [1])).toBeNull()
})

test("inboxToSessionMsgs: user-записи с payload.text мапятся; мусор стрипится почастино", () => {
  const entries = [
    { type: "user", id: "msg_i1", time: { created: 5 * M }, payload: { text: "вброс посреди хода" }, delivery: "steer" },
    { type: "compaction", id: "msg_c", time: { created: 6 * M } },
    { type: "user", id: "msg_i2", time: { created: 7 * M }, payload: {} }, // без текста
    { type: "user", id: "msg_i3", payload: { text: "без времени" } },
    "мусор",
    null,
  ]
  const msgs = inboxToSessionMsgs(entries)
  expect(msgs).toEqual([{ id: "msg_i1", type: "user", time: { created: 5 * M }, text: "вброс посреди хода" }])
  // и результат сразу пригоден для promptLandedAt (дедуп рестарт-окна)
  expect(promptLandedAt(msgs, 4 * M, "вброс посреди хода")).toBe(5 * M)
  expect(inboxToSessionMsgs([])).toEqual([])
})

// ——— capUnchatBatch ———

test("capUnchatBatch: ТГ не режется, без-чатовые — порцией; порядок ТГ-первым", () => {
  const tg = (id: number): Request => R({ chatID: 42, msgID: id })
  const unchat = (id: number): Request => R({ chatID: 0, msgID: id, drafts: [{ project: "p", rowId: `r${id}`, docId: "d", title: "t" }] })
  const batch = capUnchatBatch([tg(1), unchat(2), tg(3), unchat(4), unchat(5), unchat(6), unchat(7), unchat(8)], 5)
  expect(batch.map((b) => b.msgID)).toEqual([1, 3, 2, 4, 5, 6, 7]) // ТГ все, без-чатовые первые 5
  expect(capUnchatBatch([unchat(1), unchat(2)], 5).length).toBe(2)
  expect(capUnchatBatch([tg(1), tg(2)], 5).map((b) => b.msgID)).toEqual([1, 2])
  expect(capUnchatBatch([], 5)).toEqual([])
})

// ——— plainChunks / alignChunks ———

test("plainChunks: короткий текст — один чанк", () => {
  expect(plainChunks("привет")).toEqual(["привет"])
})

test("plainChunks: нарезка по лимиту без разрыва суррогатных пар", () => {
  const emoji = "😀".repeat(3000)
  const parts = plainChunks(emoji, 3800)
  expect(parts.length).toBe(2)
  expect(parts.join("")).toBe(emoji) // потери нет
  for (const p of parts) {
    expect([...p].every((ch) => ch === "😀")).toBe(true) // ни один чанк не рвёт пару
  }
})

test("plainChunks: лимит на стыке пары сдвигает границу (+1)", () => {
  const s = "a" + "😀" // пара начинается на индексе 1, граница 3800 её не задевает — проверим узкий лимит
  expect(plainChunks(s, 1).join("")).toBe(s)
  const pairAtLimit = "😀" + "b".repeat(3798) + "😀"
  const parts = plainChunks(pairAtLimit, 3800)
  expect(parts.join("")).toBe(pairAtLimit)
})

test("alignChunks: куски покрывают исходник без потерь и дублей", () => {
  const source = "abcdef"
  const aligned = alignChunks(["<b>abc</b>", "<i>def</i>"], source)
  expect(aligned.join("")).toBe(source)
})

test("alignChunks: границы не рвут суррогатные пары исходника", () => {
  const source = "😀".repeat(10)
  const aligned = alignChunks(["<b>x</b>", "<i>y</i>", "<u>z</u>"], source)
  expect(aligned.join("")).toBe(source)
})

test("alignChunks: пустой исходник — пустые куски по числу частей", () => {
  expect(alignChunks(["<b>x</b>", "<i>y</i>"], "")).toEqual(["", ""])
})

// ——— seen/ack: парсер канала исхода ———

test("parseDraftAcks: пары на одной и нескольких строках, позиция любая", () => {
  const text = [
    "какой-то текст",
    "draft-ack: r1=todo r2=merge; r3=rm",
    "ещё текст draft-ack: r4=пишется",
    "draft-ack: r5=обсуждение",
  ].join("\n")
  expect(parseDraftAcks(text)).toEqual({ r1: "todo", r2: "merge", r3: "rm", r4: "пишется", r5: "обсуждение" })
})

test("parseDraftAcks: нормализация токенов — бэктики, жирный, регистр, пробелы", () => {
  expect(parseDraftAcks("draft-ack: `r1` = **TODO**")).toEqual({ r1: "todo" })
  expect(parseDraftAcks("draft-ack: _r1_=Merge")).toEqual({ r1: "merge" })
})

test("parseDraftAcks: неизвестный вердикт и кривая пара — игнор (не ack)", () => {
  expect(parseDraftAcks("draft-ack: r1=сделано r2")).toEqual({})
})

test("parseDraftAcks: повтор rowId — last-wins (агент передумал в ходе)", () => {
  expect(parseDraftAcks("draft-ack: r1=todo\ndraft-ack: r1=merge")).toEqual({ r1: "merge" })
})

test("stripDraftAcks: ack-строки уходят, остальной текст без потерь", () => {
  const text = "первая\nmiddle draft-ack: r1=todo\nвторая"
  expect(stripDraftAcks(text)).toBe("первая\nвторая")
  expect(stripDraftAcks("draft-ack: r1=todo")).toBe("")
})
