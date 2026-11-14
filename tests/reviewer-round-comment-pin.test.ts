// reviewer-round-comment-pin.test.ts — пин-тест контракта «один ревью-раунд =
// один коммент» (задача 1089): отчёт раунда ревьювера (план/результат) постится
// одним сводным комментом — вердикт, находки всех секций severity и требуемое
// действие в одном сообщении; один коммент = одно уведомление владельцу (дробление
// по находке рождает N уведомлений — инстанс: план-ревью задачи 52, 5 комментов
// за 5 секунд). Контент-пин методичек: частичный конфликт мержа → тест падает
// и напоминает, что правка методички — осознанное решение. Пин фабричных
// промптов (reviewSpawnPrompt / reviewRerunPrompt / reviewCommentsPrompt) —
// каналы, которые агент читает раньше методички. Поведенческая половина (агент
// реально не дробит) — LLM-поведение: живой контроль на следующем ревью-раунде,
// не автотест (прецедент triage-zoomout-flags).
import { describe, test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { reviewSpawnPrompt, reviewRerunPrompt, reviewCommentsPrompt } from "../factory/factory-lib"

const REPO = join(import.meta.dir, "..")
const CODE_REVIEWER_MD = join(REPO, "agents", "code-quality-reviewer.md")
const PLAN_REVIEWER_MD = join(REPO, "agents", "plan-reviewer.md")

/** Контракт одного коммента — дословно в обеих методичках. */
const SINGLE_COMMENT_SIGNATURES = [
  "одним сводным комментом", // правило п. «Отчёт»
  "один вызов на раунд", // транспорт: один вызов task comment
  "одно уведомление владельцу", // инвариант уведомлений
]

/** Шапка сводки раунда + отношение к Summary — дословно в обеих методичках. */
const ROUND_HEADER_SIGNATURES = [
  "Ревью-раунд: [вердикт], N находок (X high, Y common, Z low)", // строка шапки
  "канон подсчётов", // шапка — источник чисел
  "повторяет их, не пересчитывает", // Summary повторяет шапку
]

/** Пины многострочных носителей (markdown-переносы промптов/методичек) —
 *  сверяем по нормализованному пробелу, перенос строки не рвёт сигнатуру. */
const norm = (s: string) => s.replace(/\s+/g, " ")

describe("контракт «один ревью-раунд = один коммент»: методички", () => {
  for (const [name, path] of [
    ["code-quality-reviewer.md", CODE_REVIEWER_MD],
    ["plan-reviewer.md", PLAN_REVIEWER_MD],
  ] as const) {
    test(`${name} — правило одного коммента в п. «Отчёт»`, () => {
      const md = norm(readFileSync(path, "utf-8"))
      for (const s of SINGLE_COMMENT_SIGNATURES) {
        expect(md).toContain(s)
      }
    })
    test(`${name} — шапка «Ревью-раунд:» канонизирует подсчёты, Summary повторяет`, () => {
      const md = norm(readFileSync(path, "utf-8"))
      for (const s of ROUND_HEADER_SIGNATURES) {
        expect(md).toContain(s)
      }
    })
  }
})

describe("контракт «один ревью-раунд = один коммент»: фабричные промпты", () => {
  const NO_CTX = { branch: "", worktree: "", pr: "" }

  test("reviewSpawnPrompt — отчёт одним сводным комментом (plan и result)", () => {
    for (const kind of ["plan", "result"] as const) {
      const p = norm(reviewSpawnPrompt("r1", "uwuwu", kind, NO_CTX, "reviewer"))
      expect(p).toContain("одним сводным комментом")
    }
  })

  test("reviewRerunPrompt — отчёт одним сводным комментом", () => {
    const p = norm(reviewRerunPrompt("r1", "result", NO_CTX))
    expect(p).toContain("одним сводным комментом")
  })

  test("reviewCommentsPrompt — ответ на пачку вопросов одним комментом", () => {
    const p = norm(reviewCommentsPrompt("r1", [{ id: "c1", text: "а точно?", createdAt: 1 }]))
    expect(p).toContain("одним сводным комментом")
  })
})
