// factory-stall-design-pin.test.ts — контент-пин проводки сталл-детекта на
// Design в тике фабрики (задача 1101): инлайн-гейты tickProject автотестами
// не покрываются (тик тянет реальные доски/serve/gh), а корень инцидента —
// именно в них: гейт `status === "In Progress"` исключал Design-фантом из
// санации, слот CUP держался вечно. Пин упадёт при откате гейтов или
// рефакторинге их формы — это напоминание, что правка проводки сталла —
// осознанное решение (прецедент reviewer-round-comment-pin). Поведенческая
// половина (Design-фантом реально эскалирует) — юниты stallVerdict + песочный
// DRY_RUN-тик, живой контроль — прод-лог.
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const FACTORY_TS = join(import.meta.dir, "..", "factory", "factory.ts")

/** Пины многострочных носителей — по нормализованному пробелу, перенос
 *  строки не рвёт сигнатуру. */
const norm = (s: string) => s.replace(/\s+/g, " ")

const src = norm(readFileSync(FACTORY_TS, "utf-8"))

describe("проводка сталл-детекта Design в тике (инцидент 1101)", () => {
  test("ветка зависшего хода: детекция применима к In Progress и Design", () => {
    expect(src).toContain(
      `quiet === "alive" && (row.status === "In Progress" || row.status === "Design")`,
    )
  })

  test("merged-кэш считается только для In Progress — Design-пере-вход не глушится старым позитивом", () => {
    expect(src).toContain(
      `let prMerged = row.status === "In Progress" && (tracked.mergedPrUrl ?? "") !== "" && (row.pr === "" || tracked.mergedPrUrl === row.pr)`,
    )
  })

  test("ленивая gh-проба merge — только In Progress (PR на этапе утряски не существует)", () => {
    expect(src).toContain(
      `if (stalled && !prMerged && row.status === "In Progress" && tracked.stallSignaled !== true)`,
    )
  })

  test("шапка-комментарий механики синхронизирована: пункт 5 называет оба статуса", () => {
    expect(src).toContain(
      `5. In Progress и Design: сессия жива, юзер и агент молчат дольше порога`,
    )
  })
})
