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
  worktreeByBranch,
  isValidTracked,
  reviewKindOf,
  reviewSpawnPrompt,
  reviewRerunPrompt,
  reviewCommentsPrompt,
  reviewReturnPrompt,
  routeTask,
  REVIEW_PLAN_STATUS,
  REVIEW_RESULT_STATUS,
  type FactoryState,
  type RouteFacts,
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

describe("reviewKindOf", () => {
  test("ревью-статусы → виды, остальное → null", () => {
    expect(reviewKindOf(REVIEW_PLAN_STATUS)).toBe("plan")
    expect(reviewKindOf(REVIEW_RESULT_STATUS)).toBe("result")
    expect(reviewKindOf("Design")).toBe(null)
    expect(reviewKindOf("In Progress")).toBe(null)
    expect(reviewKindOf("Review")).toBe(null)
    expect(reviewKindOf("")).toBe(null)
  })
})

describe("routeTask", () => {
  const facts = (over: Partial<RouteFacts>): RouteFacts => ({
    status: "In Progress",
    lastStatus: "In Progress",
    reviewerAlive: null,
    freshComments: [],
    ...over,
  })
  test("вход в ревью-статус без сессии → спавн", () => {
    expect(routeTask(facts({ status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: false })))
      .toEqual({ t: "spawn-reviewer", kind: "plan" })
    expect(routeTask(facts({ status: REVIEW_RESULT_STATUS, lastStatus: "In Progress", reviewerAlive: false })))
      .toEqual({ t: "spawn-reviewer", kind: "result" })
  })
  test("мёртвая ревью-сессия → спавн даже без перехода", () => {
    expect(routeTask(facts({ status: REVIEW_RESULT_STATUS, lastStatus: REVIEW_RESULT_STATUS, reviewerAlive: false })))
      .toEqual({ t: "spawn-reviewer", kind: "result" })
  })
  test("живая сессия + вход/смена вида → доревью", () => {
    expect(routeTask(facts({ status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: true })))
      .toEqual({ t: "rerun-reviewer", kind: "plan" })
    // смена вида plan → result в той же сессии
    expect(routeTask(facts({ status: REVIEW_RESULT_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true })))
      .toEqual({ t: "rerun-reviewer", kind: "result" })
    // повторный раунд фиксов: вышел и вернулся в тот же вид
    expect(routeTask(facts({ status: REVIEW_RESULT_STATUS, lastStatus: "In Progress", reviewerAlive: true })))
      .toEqual({ t: "rerun-reviewer", kind: "result" })
  })
  test("живая сессия в том же статусе: комменты юзера → ревьюверу, тишина → noop", () => {
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true,
      freshComments: [c("x", "вопрос", 1)],
    }))).toEqual({ t: "comments-to-reviewer" })
    expect(routeTask(facts({ status: REVIEW_PLAN_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true })))
      .toEqual({ t: "noop" })
  })
  test("liveness неизвестна → сессию считаем живой (спавнов не плодим)", () => {
    expect(routeTask(facts({ status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: null })))
      .toEqual({ t: "rerun-reviewer", kind: "plan" })
  })
  test("возврат из ревью: Design/In Progress → девелоперу, Review/NA → noop", () => {
    expect(routeTask(facts({ status: "In Progress", lastStatus: REVIEW_PLAN_STATUS })))
      .toEqual({ t: "return-to-dev" })
    expect(routeTask(facts({ status: "Design", lastStatus: REVIEW_PLAN_STATUS })))
      .toEqual({ t: "return-to-dev" })
    expect(routeTask(facts({ status: "Review", lastStatus: REVIEW_RESULT_STATUS })))
      .toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "Needs attention", lastStatus: REVIEW_PLAN_STATUS })))
      .toEqual({ t: "noop" })
  })
  test("ручной перевод живой задачи в Design → возврат девелоперу (перепланируй)", () => {
    expect(routeTask(facts({ status: "Design", lastStatus: "In Progress" })))
      .toEqual({ t: "return-to-dev" })
  })
  test("спокойные статусы без переходов → noop", () => {
    expect(routeTask(facts({ status: "Design", lastStatus: "Design" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "Design", lastStatus: "" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "In Progress", lastStatus: "In Progress" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Design", freshComments: [c("x", "привет", 1)] })))
      .toEqual({ t: "noop" }) // комменты девелоперу доставляет пайп ниже маршрутизации
  })
})

describe("reviewSpawnPrompt", () => {
  const ctx = { branch: "feat/x", worktree: "/home/x/origa-task-x", pr: "https://github.com/x/pull/1" }
  test("plan: вид, канал чтения, переходы approve/request_changes", () => {
    const p = reviewSpawnPrompt("r1", "uwuwu", "plan", { branch: "", worktree: "", pr: "" })
    expect(p).toContain("ревью плана")
    expect(p).toContain("uwuwu-cli task get r1 --project uwuwu")
    expect(p).toContain("approve → In Progress")
    expect(p).toContain("request_changes → Design")
    expect(p).not.toContain("request_changes → In Progress")
  })
  test("result: ветка/ворктри/PR и переходы результата", () => {
    const p = reviewSpawnPrompt("r2", "origa", "result", ctx)
    expect(p).toContain("ревью результата")
    expect(p).toContain(ctx.worktree)
    expect(p).toContain(ctx.branch)
    expect(p).toContain(ctx.pr)
    expect(p).toContain("approve → Review")
    expect(p).toContain("request_changes → In Progress")
    expect(p).not.toContain("request_changes → Design")
  })
})

describe("reviewRerunPrompt / reviewCommentsPrompt / reviewReturnPrompt", () => {
  test("rerun: статус раунда + переходы + контекст ветки", () => {
    const p = reviewRerunPrompt("r1", "result", { branch: "b", worktree: "/w", pr: "https://pr/1" })
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("устранение своих прошлых замечаний")
    expect(p).toContain("approve → Review")
    expect(p).toContain("https://pr/1")
  })
  test("comments: цитаты юзера + ответ комментом", () => {
    const p = reviewCommentsPrompt("r1", [c("a", "а точно?", 1)])
    expect(p).toContain("ревью-статус")
    expect(p).toContain("а точно?")
    expect(p).toContain("uwuwu-cli task comment")
  })
  test("return: девелопер перечитывает задачу и действует по вердикту", () => {
    const p = reviewReturnPrompt("r1")
    expect(p).toContain("вернулась к разработке")
    expect(p).toContain("task get")
    expect(p).toContain("замечания ревьювера")
    expect(p).toContain("перепланируй")
  })
})

describe("isValidTracked: ревью-поля", () => {
  const base = {
    sessionID: "ses_1", worktree: "", branch: "",
    bodyDoc: "d", title: "t", project: "uwuwu",
    lastCommentAt: 0, lastCommentId: "", createdAt: 1, attempts: 0,
  }
  test("reviewSessionID: отсутствует / пусто / ses_ — валидны", () => {
    expect(isValidTracked(base)).toBe(true)
    expect(isValidTracked({ ...base, reviewSessionID: "" })).toBe(true)
    expect(isValidTracked({ ...base, reviewSessionID: "ses_r1" })).toBe(true)
  })
  test("reviewSessionID мусор и reviewAttempts не-число — невалидны", () => {
    expect(isValidTracked({ ...base, reviewSessionID: "junk" })).toBe(false)
    expect(isValidTracked({ ...base, reviewAttempts: "1" })).toBe(false)
    expect(isValidTracked({ ...base, reviewAttempts: 2 })).toBe(true)
  })
})

describe("worktreeByBranch", () => {
  const porcelain = [
    "worktree /home/x/origa",
    "HEAD abc",
    "branch refs/heads/master",
    "",
    "worktree /home/x/origa-wt-commits",
    "HEAD def",
    "branch refs/heads/work/auto-atomic-commits",
    "",
    "worktree /home/x/origa-wt-x",
    "HEAD 123",
    "branch refs/heads/fix-x",
    "",
  ].join("\n")
  test("точное совпадение имени ветки", () => {
    expect(worktreeByBranch(porcelain, "fix-x")).toBe("/home/x/origa-wt-x")
    expect(worktreeByBranch(porcelain, "work/auto-atomic-commits")).toBe("/home/x/origa-wt-commits")
    expect(worktreeByBranch(porcelain, "master")).toBe("/home/x/origa")
  })
  test("суффикс-подстрока чужой ветки не матчится", () => {
    expect(worktreeByBranch(porcelain, "commits")).toBe("")
    expect(worktreeByBranch(porcelain, "x")).toBe("")
    expect(worktreeByBranch(porcelain, "auto-atomic-commits")).toBe("")
  })
  test("пустая ветка и отсутствие ветки — пустой результат", () => {
    expect(worktreeByBranch(porcelain, "")).toBe("")
    expect(worktreeByBranch("worktree /a\nHEAD z\nbare", "nope")).toBe("")
  })
})
