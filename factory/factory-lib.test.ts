// factory-lib.test.ts — юнит-тесты чистых функций uwuwu-factory.
import { describe, expect, test } from "bun:test"
import {
  commentText,
  newUserComments,
  bodyEditPrompt,
  spawnPrompt,
  untrackedTasks,
  isUserComment,
  snapshotText,
  worktreePaths,
  isValidTracked,
  type FactoryState,
  type TaskComment,
  type TaskRef,
} from "./factory-lib"

const c = (id: string, text: string, at: number): TaskComment => ({ id, text, createdAt: at })

describe("isUserComment", () => {
  test("маркер агента и служебные — не юзерские", () => {
    expect(isUserComment(c("1", "🤖 отчёт", 1))).toBe(false)
    expect(isUserComment(c("2", "[factory] сессия потеряна", 1))).toBe(false)
    expect(isUserComment(c("3", "   ", 1))).toBe(false)
    expect(isUserComment(c("4", "сделай так", 1))).toBe(true)
    expect(isUserComment(c("5", "  🤖 с отступом", 1))).toBe(false)
  })
})

describe("newUserComments", () => {
  const comments = [
    c("a", "🤖 стартую", 100),
    c("b", "первый вопрос", 200),
    c("d", "🤖 ответ юзеру", 300),
    c("e", "второй вопрос", 300), // та же мс — порядок по id
    c("f", "[factory] noise", 400),
    c("g", "третий", 500),
  ]
  test("пустая граница — все юзерские по порядку", () => {
    const fresh = newUserComments(comments, { at: 0, id: "" })
    expect(fresh.map((x) => x.id)).toEqual(["b", "e", "g"])
  })
  test("позиционная граница по юзерскому id", () => {
    const fresh = newUserComments(comments, { at: 300, id: "e" })
    expect(fresh.map((x) => x.id)).toEqual(["g"])
  })
  test("граница на агентском комменте — строгий фоллбек по времени", () => {
    // d (🤖) в юзерском списке нет → позиция не найдена → createdAt > 300
    const fresh = newUserComments(comments, { at: 300, id: "d" })
    expect(fresh.map((x) => x.id)).toEqual(["g"])
  })
  test("пограничный удалён — строгий фоллбек по времени", () => {
    const fresh = newUserComments([c("b", "q", 200), c("g", "t", 500)], { at: 300, id: "d" })
    expect(fresh.map((x) => x.id)).toEqual(["g"])
  })
})

describe("untrackedTasks", () => {
  test("только новые rowId", () => {
    const rows: TaskRef[] = [
      { rowId: "r1", bodyDoc: "d1", title: "a", status: "Todo", pr: "", branch: "" },
      { rowId: "r2", bodyDoc: "d2", title: "b", status: "Todo", pr: "", branch: "" },
    ]
    const state: FactoryState = { tasks: { r1: {} as never } }
    expect(untrackedTasks(rows, state).map((r) => r.rowId)).toEqual(["r2"])
  })
})

describe("snapshotText", () => {
  test("конкатенация дельт вложенного дерева", () => {
    const content = {
      snapshot: {
        blocks: {
          props: { title: { delta: [] } },
          children: [
            { props: { text: { delta: [{ insert: "при" }, { insert: "вет" }] } }, children: [] },
            { props: {}, children: [{ props: { text: { delta: [{ insert: "!" }] } }, children: [] }] },
          ],
        },
      },
    }
    expect(snapshotText(content)).toBe("привет!")
  })
  test("пустое — пустая строка", () => {
    expect(snapshotText({})).toBe("")
    expect(snapshotText(null)).toBe("")
  })
})

describe("worktreePaths", () => {
  test("ведущий дефис row-id sanitized", () => {
    const { branch, worktree } = worktreePaths("/root", "-5BF9NMBXZ")
    expect(branch).toBe("work/task-5BF9NMBXZ")
    expect(worktree).toBe("/root/task-5BF9NMBXZ")
  })
  test("мусорные символы вырезаются", () => {
    expect(worktreePaths("/r", "abc$#@!def").branch).toMatch(/^work\/task-[a-zA-Z0-9_-]+$/)
  })
})

describe("isValidTracked", () => {
  const base = {
    sessionID: "ses_abc", worktree: "/wt/task-x", branch: "work/task-x",
    bodyDoc: "d1", title: "t", project: "uwuwu", lastCommentAt: 1, lastCommentId: "i", createdAt: 2, attempts: 0,
  }
  test("полный формат валиден", () => {
    expect(isValidTracked(base)).toBe(true)
  })
  test("чистый карантин валиден", () => {
    expect(isValidTracked({ ...base, sessionID: "", worktree: "", branch: "" })).toBe(true)
  })
  test("гибрид (сессия умерла, пути живы) валиден", () => {
    expect(isValidTracked({ ...base, sessionID: "" })).toBe(true)
  })
  test("чужие пути и не-ses сессии — мимо", () => {
    expect(isValidTracked({ ...base, worktree: "relative" })).toBe(false)
    expect(isValidTracked({ ...base, sessionID: "gh_1" })).toBe(false)
    expect(isValidTracked({ ...base, lastCommentAt: "1" })).toBe(false)
  })
})

describe("spawnPrompt", () => {
  test("контракт будильника: задача/проект/репо + указание на AGENTS.md", () => {
    const p = spawnPrompt("-5BF9", "uwuwu", "yurvon-screamo/uwuwu-agent")
    expect(p).toContain("Задача -5BF9")
    expect(p).toContain("проект uwuwu")
    expect(p).toContain("yurvon-screamo/uwuwu-agent")
    expect(p).toContain("AGENTS.md")
    expect(p).toContain("task get")
    expect(p).not.toContain("🤖")
  })
  test("пайпа в промпте нет — он в глобальных инструкциях", () => {
    const p = spawnPrompt("r1", "origa", "yurvon-screamo/origa")
    expect(p).not.toContain("ПАЙП")
    expect(p).not.toContain("=== ТЕЛО ЗАДАЧИ ===")
  })
})

describe("bodyEditPrompt", () => {
  test("контракт: перечитай тело + ответ комментом", () => {
    const p = bodyEditPrompt("r1")
    expect(p).toContain("отредактировал ТЕЛО задачи r1")
    expect(p).toContain("task get")
    expect(p).not.toContain("🤖")
  })
})

describe("commentText — контекст ответов", () => {
  const pageReply = {
    mode: "page",
    preview: "A. Сжать до одной строки-вердикта — рекомендация",
    snapshot: { blocks: { props: { text: { delta: [{ insert: "В целом нормвариант" }] } }, children: [] } },
  }
  const plain = { snapshot: { blocks: { props: { text: { delta: [{ insert: "🤖 отчёт" }] } }, children: [] } } }
  test("page-режим (ответ цитатой) несёт якорь preview", () => {
    const t = commentText(pageReply)
    expect(t).toContain("в ответ на: «A. Сжать")
    expect(t).toContain("В целом нормвариант")
  })
  test("обычный коммент — без якоря", () => {
    expect(commentText(plain)).toBe("🤖 отчёт")
  })
})
