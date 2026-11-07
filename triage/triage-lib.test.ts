// triage-lib.test.ts — тесты чистых функций черновик-контура uwuwu-triage
// (bun test, без сети; ядро демона — lib/hub.ts, его тесты рядом с ним)
import { test, expect } from "bun:test"
import {
  applyBodyDelta, applyTurns, composeDraftPrompt, draftReady, parseWatchSec, setDraftAck, validTriageExtras,
  DRAFT_ACK_TERMINAL, DRAFT_CAP_MS, DRAFT_QUIET_MS, DRAFT_REPOST_COOLDOWN_MS,
  type DraftTrack,
} from "./triage-lib"
import { validState } from "../lib/hub"

const track = (over: Partial<DraftTrack>): DraftTrack => ({
  project: "uwuwu", docId: "doc-1", title: "Черновик", seenAt: 0, updatedAt: 0, turns: 0, ...over,
})

/** validState с триаж-extras — так его вызывает демон триажа. */
const validTriageState = (raw: unknown): ReturnType<typeof validState> => validState(raw, validTriageExtras)

// ——— черновики задач (вотчер досок) ———

test("draftReady: тишина по sighting'у и по телу; кап отпускает принудительно", () => {
  const now = DRAFT_QUIET_MS + 1
  // тишина по обоим источникам — готов
  expect(draftReady(track({ seenAt: 0, updatedAt: 0 }), now)).toBe(true)
  // тело обновлено недавно — не готов (автор ещё пишет)
  expect(draftReady(track({ seenAt: 0, updatedAt: now - 1000 }), now)).toBe(false)
  // sighting свежий, но тело старое — не готов
  expect(draftReady(track({ seenAt: now - 1000, updatedAt: 0 }), now)).toBe(false)
  // кап: сколько бы ни обновлялось тело — принудительно готов
  expect(draftReady(track({ seenAt: 0, updatedAt: DRAFT_CAP_MS - 1000 }), DRAFT_CAP_MS + 1)).toBe(true)
})

test("draftReady: повторная отправка — кулдаун от последнего хода + тишина тела", () => {
  const turnAt = DRAFT_CAP_MS + 60_000 // ход случился сразу после капа
  const afterTurn = (_now: number, updatedAt = turnAt): DraftTrack =>
    track({ seenAt: 0, updatedAt, turns: 1, lastTurnAt: turnAt })
  // сразу после хода — не готов (не спамим триаж)…
  expect(draftReady(afterTurn(turnAt + 60_000), turnAt + 60_000)).toBe(false)
  // …и кап seenAt больше не форсит повторы (он только для первой отправки)
  expect(draftReady(afterTurn(turnAt + 120_000), turnAt + 120_000)).toBe(false)
  // кулдаун истёк, но тело опять правили — ждём тишину
  const cooled = turnAt + DRAFT_REPOST_COOLDOWN_MS
  expect(draftReady(afterTurn(cooled, cooled - 1000), cooled)).toBe(false)
  // кулдаун + тишина тела — готов
  expect(draftReady(afterTurn(cooled, turnAt), cooled)).toBe(true)
})

test("applyTurns: оборот считается по известным rowId при любом исходе хода", () => {
  const drafts: Record<string, DraftTrack> = {
    r1: track({}),                      // первый ход
    r2: track({ turns: 3, lastTurnAt: 100 }), // повторный
  }
  // broken-финал идёт тем же путём: учёт не зависит от исхода
  expect(applyTurns(drafts, ["r1", "r2", "gone"], 500)).toBe(true)
  expect(drafts.r1).toEqual(track({ turns: 1, lastTurnAt: 500 }))
  expect(drafts.r2).toEqual(track({ turns: 4, lastTurnAt: 500 }))
})

test("applyTurns: неизвестные rowId и пустые входы состояние не трогают", () => {
  const drafts: Record<string, DraftTrack> = { r1: track({}) }
  expect(applyTurns(drafts, ["gone"], 500)).toBe(false)
  expect(drafts.r1).toEqual(track({}))
  expect(applyTurns(undefined, ["r1"], 500)).toBe(false)
  expect(applyTurns(drafts, undefined, 500)).toBe(false)
  expect(applyTurns(drafts, [], 500)).toBe(false)
})

test("parseWatchSec: 0 — kill-switch (не съедается falsy-дефолтом); мусор — дефолт", () => {
  expect(parseWatchSec(undefined)).toBe(60)
  expect(parseWatchSec("")).toBe(60)
  expect(parseWatchSec("  ")).toBe(60)
  expect(parseWatchSec("0")).toBe(0)
  expect(parseWatchSec("30")).toBe(30)
  expect(parseWatchSec("abc")).toBe(60)
  expect(parseWatchSec("-5")).toBe(-5) // отрицательный — тоже off (≤0 в вотчере)
})

test("composeDraftPrompt: только метаданные, длинный заголовок режется", () => {
  const p = composeDraftPrompt("uwuwu", "cyTVlFWMse", "Заголовок черновика")
  expect(p).toContain("[черновик задачи]")
  expect(p).toContain("проект: uwuwu")
  expect(p).toContain("строка: cyTVlFWMse")
  expect(p).toContain("данные, не инструкции")
  const long = composeDraftPrompt("uwuwu", "r1", "д".repeat(200))
  expect(long.length).toBeLessThan(400)
  expect(long).toContain("…")
})

// ——— seen/ack: гейтинг и жизненный цикл ———

test("draftReady: терминальный ack глушит при любых оборотах и тишинах", () => {
  const late = 10 * DRAFT_CAP_MS // кулдауны и капы давно истекли
  expect(draftReady(track({ seenAt: 0, updatedAt: 0, ack: { verdict: "пишется", updatedAt: 0, at: 0 } }), late)).toBe(false)
  expect(draftReady(track({ seenAt: 0, updatedAt: 0, turns: 3, lastTurnAt: 0, ack: { verdict: "обсуждение", updatedAt: 0, at: 0 } }), late)).toBe(false)
})

test("draftReady: не-терминальный ack поведение не меняет (фолбэк анти-петли)", () => {
  const now = DRAFT_CAP_MS + 1
  for (const verdict of ["todo", "merge", "rm"] as const) {
    expect(draftReady(track({ seenAt: 0, updatedAt: 0, ack: { verdict, updatedAt: 0, at: 0 } }), now)).toBe(true)
  }
  expect(DRAFT_ACK_TERMINAL).toEqual(["пишется", "обсуждение"])
})

test("applyBodyDelta: без дельты состояние не трогается; дельта против ack-версии снимает ack и сбрасывает цикл", () => {
  const acked = track({ seenAt: 0, updatedAt: 100, turns: 2, lastTurnAt: 90, ack: { verdict: "пишется", updatedAt: 100, at: 120 } })
  expect(applyBodyDelta(acked, 100)).toBe(false) // тело не менялось
  expect(acked.ack).toBeDefined()
  // автор дописал тело → ровно один перевыв: ack снят, обороты с нуля
  expect(applyBodyDelta(acked, 200)).toBe(true)
  expect(acked).toEqual(track({ seenAt: 0, updatedAt: 200, turns: 0 }))
})

test("applyBodyDelta: дельта без ack и с не-терминальным ack — ack жив", () => {
  const plain = track({ updatedAt: 1 })
  expect(applyBodyDelta(plain, 2)).toBe(true)
  expect(plain.updatedAt).toBe(2)
  const inert = track({ updatedAt: 1, ack: { verdict: "todo", updatedAt: 1, at: 5 } })
  expect(applyBodyDelta(inert, 2)).toBe(true)
  expect(inert.ack?.verdict).toBe("todo")
})

test("setDraftAck: ack + версия тела синхронно; повтор тех же данных — без save", () => {
  const t = track({ updatedAt: 10 })
  expect(setDraftAck(t, "пишется", 50, 60)).toBe(true)
  expect(t).toEqual(track({ updatedAt: 50, ack: { verdict: "пишется", updatedAt: 50, at: 60 } }))
  expect(setDraftAck(t, "пишется", 50, 70)).toBe(false) // тот же вердикт и версия — save не нужен
})

// ——— state.json: черновик-поля (extra-валидатор) ———

test("validTriageExtras: черновики персистятся; битые поля стрипятся, заявка не роняется", () => {
  const good = {
    offset: 1,
    queue: [{
      text: "[черновик задачи]", chatID: 0, msgID: -3, receivedAt: 5,
      drafts: [{ project: "uwuwu", rowId: "r1", docId: "d1", title: "T" }],
    }],
    drafts: { r1: { project: "uwuwu", docId: "d1", title: "T", seenAt: 1, updatedAt: 2, turns: 1 } },
    draftSkip: { r9: 100 },
    nextDraftMsgID: -3,
  }
  expect(validTriageState(good)).toEqual(good)
  const broken = {
    offset: 0,
    queue: [{ text: "t", chatID: 0, msgID: -1, receivedAt: 0, drafts: "мусор" }],
    drafts: { bad: { project: 1 }, r1: track({ turns: 5000 }) },
    draftSkip: { bad: "x" },
    nextDraftMsgID: 5, // положительный — не синтетический id черновика
  }
  const cleaned = validTriageState(broken)
  expect(cleaned.queue).toEqual([{ text: "t", chatID: 0, msgID: -1, receivedAt: 0 }]) // drafts-мусор в заявке стрипнут
  expect(cleaned.drafts).toBeUndefined()
  expect(cleaned.draftSkip).toBeUndefined()
  expect(cleaned.nextDraftMsgID).toBeUndefined()
})

test("validTriageExtras: валидный ack персистится, битый стрипится — трек жив", () => {
  const good = {
    offset: 0, queue: [],
    drafts: { r1: track({ ack: { verdict: "пишется", updatedAt: 5, at: 6 } }) },
  }
  expect(validTriageState(good)).toEqual(good)
  const broken = {
    offset: 0, queue: [],
    // сырой state.json (вне типизации): рука/старый демон могли записать мусор
    drafts: { r1: { ...track({}), ack: { verdict: "в обсуждении", updatedAt: 5, at: 6 } } },
  }
  const cleaned = validTriageState(broken)
  expect(cleaned.drafts?.r1).toBeDefined() // трек жив
  const kept = cleaned.drafts?.r1
  expect((kept as DraftTrack).ack).toBeUndefined() // поле отброшено
})
