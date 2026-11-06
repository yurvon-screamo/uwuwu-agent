// factory-lib.test.ts — юнит-тесты чистых функций uwuwu-factory.
import { describe, expect, test } from "bun:test"
import {
  commentText,
  conflictFixPrompt,
  continuePrompt,
  kickoffModeOf,
  newUserComments,
  bodyEditPrompt,
  promptForStart,
  prMergeableOf,
  mergeablePollStep,
  spawnPrompt,
  untrackedTasks,
  isUserComment,
  snapshotText,
  worktreePaths,
  worktreeByBranch,
  runBatch,
  parseProjects,
  formatPassSummary,
  adoptBodyDoc,
  pathSafe,
  type ProjectCfg,
  isValidTracked,
  rowMissingAction,
  reviewKindOf,
  reviewSpawnPrompt,
  reviewRerunPrompt,
  reviewCommentsPrompt,
  reviewReturnPrompt,
  routeTask,
  REVIEW_PLAN_STATUS,
  REVIEW_RESULT_STATUS,
  stallMinutesFromEnv,
  lastActivityOf,
  stallVerdict,
  type FactoryState,
  type RouteFacts,
  type StallFacts,
  type TaskComment,
  type TaskRef,
  type TrackedTask,
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
  test("missingSince: строка и отсутствие — валидны, чужой тип — мимо", () => {
    expect(isValidTracked({ ...base, missingSince: "2026-11-05T12:00:00.000Z" })).toBe(true)
    expect(isValidTracked({ ...base, missingSince: undefined })).toBe(true)
    expect(isValidTracked({ ...base, missingSince: 42 })).toBe(false)
  })
  test("stallSignaled: boolean и отсутствие — валидны, не-boolean — мимо", () => {
    expect(isValidTracked({ ...base, stallSignaled: true })).toBe(true)
    expect(isValidTracked({ ...base, stallSignaled: undefined })).toBe(true)
    expect(isValidTracked(base)).toBe(true)
    expect(isValidTracked({ ...base, stallSignaled: "yes" })).toBe(false)
  })
})

describe("rowMissingAction — двухпроходная чистка", () => {
  test("первая пропажа — пометка, повторная — чистка", () => {
    expect(rowMissingAction(undefined, false)).toBe("mark")
    expect(rowMissingAction("2026-11-05T12:00:00.000Z", false)).toBe("clean")
  })
  test("строка вернулась — пометка снимается; без пометки — hold", () => {
    expect(rowMissingAction("2026-11-05T12:00:00.000Z", true)).toBe("clear")
    expect(rowMissingAction(undefined, true)).toBe("hold")
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
  test("design-режим (дефолт): утряска с «Ревью плана» на месте", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent")
    expect(p).toContain("Стадия Design")
    expect(p).toContain("План утрясан")
    expect(p).toContain("«Ревью плана»")
  })
  test("implement-режим (fast-track): без предписания план-ревью, сразу реализация → «Ревью результата»", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement")
    expect(p).toContain("In Progress")
    expect(p).toContain("пропускаешь")
    expect(p).toContain("«Ревью результата»")
    expect(p).toContain("task get")
    // предписание план-ревью («План утрясан → … «Ревью плана»») — только в design-режиме
    expect(p).not.toContain("План утрясан")
    expect(p).not.toContain("🤖")
  })
})

describe("continuePrompt", () => {
  test("рестарт без предписания стадии, с якорем на пайп AGENTS.md", () => {
    const p = continuePrompt("r1", "origa", "yurvon-screamo/origa")
    expect(p).toContain("Задача r1")
    expect(p).toContain("перезапущена")
    expect(p).toContain("AGENTS.md")
    expect(p).toContain("task get")
    expect(p).not.toContain("Стадия Design")
    expect(p).not.toContain("«Ревью плана»")
    expect(p).not.toContain("🤖")
  })
})

describe("kickoffModeOf", () => {
  test("In Progress → implement (стадия реализации)", () => {
    expect(kickoffModeOf("In Progress")).toBe("implement")
  })
  test("остальные статусы → design (safe default)", () => {
    expect(kickoffModeOf("Design")).toBe("design")
    expect(kickoffModeOf("Needs attention")).toBe("design")
    expect(kickoffModeOf("")).toBe("design")
    expect(kickoffModeOf(REVIEW_PLAN_STATUS)).toBe("design")
    expect(kickoffModeOf(REVIEW_RESULT_STATUS)).toBe("design")
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
  test("фаст-трек трекенной задачи: перевод владельца в In Progress из любого другого статуса → девелоперу", () => {
    // прод-факт 06.11: трек на «Review» с конфликтным PR, владелец флипает в
    // In Progress — сигнал «вмешайся и почини»; до фикса маршрутизация молчала
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Review" })))
      .toEqual({ t: "return-to-dev" })
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Needs attention" })))
      .toEqual({ t: "return-to-dev" })
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Design" })))
      .toEqual({ t: "return-to-dev" })
    // Todo/Draft — тоже fast-track-семантика (любой исходный статус)
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Todo" })))
      .toEqual({ t: "return-to-dev" })
    // флип с комментом владельца — тот же исход: комменты съедает
    // consumeComments после доставки (двойной промпт не рождается)
    expect(routeTask(facts({ status: "In Progress", lastStatus: "Review", freshComments: [c("x", "почини", 1)] })))
      .toEqual({ t: "return-to-dev" })
  })
  test("спокойные статусы без переходов → noop", () => {
    expect(routeTask(facts({ status: "Design", lastStatus: "Design" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "Design", lastStatus: "" }))).toEqual({ t: "noop" })
    // In Progress без смены статуса — не edge: комменты доставляет пайп ниже
    expect(routeTask(facts({ status: "In Progress", lastStatus: "In Progress" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "In Progress", lastStatus: "In Progress", freshComments: [c("x", "привет", 1)] })))
      .toEqual({ t: "noop" })
    // свежий карантин (фейл спавна, lastStatus не писался) — не считаем переводом
    expect(routeTask(facts({ status: "In Progress", lastStatus: "" }))).toEqual({ t: "noop" })
  })
})

describe("conflictFixPrompt", () => {
  test("конфликт-kickoff: PR, путь починки и возврат в «Ревью результата», без Design-стадии", () => {
    const p = conflictFixPrompt("r1", "https://github.com/x/pull/9")
    expect(p).toContain("r1")
    expect(p).toContain("https://github.com/x/pull/9")
    expect(p).toContain("конфликт")
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("mergeable")
    expect(p).not.toContain("Стадия Design")
    expect(p).not.toContain("Ревью плана")
  })
})

describe("promptForStart: единственная точка выбора стартового промпта", () => {
  const args = ["r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "https://github.com/x/pull/9"] as const
  test("conflict-fix → конфликт-kickoff", () => {
    const p = promptForStart("conflict-fix", ...args)
    expect(p).toBe(conflictFixPrompt("r1", "https://github.com/x/pull/9"))
  })
  test("continue → рестарт без предписания стадии", () => {
    const p = promptForStart("continue", ...args)
    expect(p).toBe(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent"))
    expect(p).not.toContain("Стадия Design")
  })
  test("implement/design → spawnPrompt соответствующего режима", () => {
    expect(promptForStart("implement", ...args)).toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement"))
    expect(promptForStart("design", ...args)).toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "design"))
  })
})

describe("prMergeableOf: парс gh pr view --json mergeable", () => {
  test("CONFLICTING/MERGEABLE пробрасываются, остальное и мусор → UNKNOWN", () => {
    expect(prMergeableOf('{"mergeable":"CONFLICTING"}')).toBe("CONFLICTING")
    expect(prMergeableOf('{"mergeable":"MERGEABLE"}')).toBe("MERGEABLE")
    expect(prMergeableOf('{"mergeable":"UNKNOWN"}')).toBe("UNKNOWN")
    expect(prMergeableOf('{"mergeable":null}')).toBe("UNKNOWN")
    expect(prMergeableOf("{}")).toBe("UNKNOWN")
    expect(prMergeableOf("не json")).toBe("UNKNOWN")
    expect(prMergeableOf("")).toBe("UNKNOWN")
  })
})

describe("mergeablePollStep: политика опроса ленивого mergeable", () => {
  test("финальные вердикты на любой попытке", () => {
    expect(mergeablePollStep("CONFLICTING", 0, 4)).toEqual({ t: "conflict" })
    expect(mergeablePollStep("CONFLICTING", 3, 4)).toEqual({ t: "conflict" })
    expect(mergeablePollStep("MERGEABLE", 0, 4)).toEqual({ t: "clear" })
    expect(mergeablePollStep("MERGEABLE", 3, 4)).toEqual({ t: "clear" })
  })
  test("UNKNOWN переспрашивается до исчерпания попыток, затем мягкая деградация", () => {
    expect(mergeablePollStep("UNKNOWN", 0, 4)).toEqual({ t: "retry" })
    expect(mergeablePollStep("UNKNOWN", 1, 4)).toEqual({ t: "retry" })
    expect(mergeablePollStep("UNKNOWN", 2, 4)).toEqual({ t: "retry" })
    expect(mergeablePollStep("UNKNOWN", 3, 4)).toEqual({ t: "degrade" })
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
    // фаст-трек владельца — тот же возврат: продолжай/почини без Design
    expect(p).toContain("In Progress")
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

// ——— спавн-волна: runBatch ———

describe("runBatch: пачка с ограничением конкурентности", () => {
  test("все элементы обработаны, конкурентность не выше лимита", async () => {
    // Arrange
    const processed: number[] = []
    let active = 0
    let maxActive = 0
    const worker = async (n: number): Promise<void> => {
      active++
      maxActive = Math.max(maxActive, active)
      await new Promise((r) => setTimeout(r, 5))
      processed.push(n)
      active--
    }

    // Act
    await runBatch([1, 2, 3, 4, 5, 6, 7], worker, 3)

    // Assert
    expect(processed.sort()).toEqual([1, 2, 3, 4, 5, 6, 7])
    expect(maxActive).toBeLessThanOrEqual(3)
  })

  test("фейл одного элемента не останавливает остальных, ошибка перевыбрасывается", async () => {
    // Arrange
    const processed: number[] = []
    const worker = async (n: number): Promise<void> => {
      await new Promise((r) => setTimeout(r, 1))
      if (n === 2) throw new Error("boom")
      processed.push(n)
    }

    // Act / Assert
    await expect(runBatch([1, 2, 3], worker, 2)).rejects.toThrow("boom")
    expect(processed.sort()).toEqual([1, 3])
  })

  test("пустая пачка и лимит больше пачки — без ошибок", async () => {
    const done: number[] = []
    await runBatch([], async () => { done.push(0) }, 4)
    await runBatch([1], async (n) => { done.push(n) }, 10)
    expect(done).toEqual([1])
  })
})

// ——— конфиг проектов: parseProjects ———

describe("parseProjects: env-оверрайд UWUWU_FACTORY_PROJECTS_JSON", () => {
  const fallback: Record<string, ProjectCfg> = {
    uwuwu: { board: "Задачи uwuwu", repo: "r", local: "/l" },
  }

  test("нет env — дефолт без изменений", () => {
    expect(parseProjects(undefined, fallback)).toEqual(fallback)
  })

  test("пустая строка — дефолт (сброс env без unset)", () => {
    expect(parseProjects("  ", fallback)).toEqual(fallback)
  })

  test("валидный JSON — конфиг целиком из env (тестовая доска)", () => {
    const raw = JSON.stringify({ test: { board: "Доска тест", repo: "repo", local: "/tmp/repo" } })
    expect(parseProjects(raw, fallback)).toEqual({
      test: { board: "Доска тест", repo: "repo", local: "/tmp/repo" },
    })
  })

  test("не-JSON — внятная ошибка с именем env", () => {
    expect(() => parseProjects("{nope", fallback)).toThrow(/UWUWU_FACTORY_PROJECTS_JSON.*не JSON/)
  })

  test("проект без обязательных полей — ошибка с именем проекта", () => {
    const raw = JSON.stringify({ test: { board: "b", repo: "r" } })
    expect(() => parseProjects(raw, fallback)).toThrow(/test.*board\/repo\/local/)
  })

  test("массив вместо объекта — отказ, не тихий пустой конфиг", () => {
    expect(() => parseProjects("[]", fallback)).toThrow(/объект/)
  })
})

// ——— сводка прохода ———

describe("formatPassSummary: одна строка о проходе", () => {
  test("полная сводка с фейлами перечисляет причины", () => {
    const line = formatPassSummary("uwuwu", {
      devSpawned: 4, reviewSpawned: 1, wakeups: 2, cleanups: 1, watched: 12,
      failures: ["20261105ab: сессия мертва после спавна"],
    })
    expect(line).toBe(
      "проход uwuwu: спавн dev=4 ревью=1, будильники=2, чистки=1, наблюдение=12" +
      ", фейлы=1 (20261105ab: сессия мертва после спавна)")
  })

  test("тихий тик без событий всё равно виден (наблюдение=0)", () => {
    const line = formatPassSummary("origa", {
      devSpawned: 0, reviewSpawned: 0, wakeups: 0, cleanups: 0, watched: 0, failures: [],
    })
    expect(line).toBe("проход origa: спавн dev=0 ревью=0, будильники=0, чистки=0, наблюдение=0")
  })
})

describe("parseProjects: семантическая валидация", () => {
  const fallback: Record<string, ProjectCfg> = {
    uwuwu: { board: "Задачи uwuwu", repo: "r", local: "/l" },
  }

  test("пустое поле — отказ с именем проекта", () => {
    const raw = JSON.stringify({ test: { board: "  ", repo: "r", local: "/l" } })
    expect(() => parseProjects(raw, fallback)).toThrow(/test.*пустыми/)
  })

  test("пустой объект — отказ: демон без проектов молча не работает", () => {
    expect(() => parseProjects("{}", fallback)).toThrow(/пуст/)
  })
})

// ——— сигнал о застое: env-парсер / парсер сообщений / вердикт ———

describe("stallMinutesFromEnv: UWUWU_FACTORY_STALL_MIN", () => {
  test("таблица: дефолт 30 на пустоту/мусор, валидное число — оно", () => {
    expect(stallMinutesFromEnv(undefined)).toBe(30)
    expect(stallMinutesFromEnv("")).toBe(30)
    expect(stallMinutesFromEnv("  ")).toBe(30)
    expect(stallMinutesFromEnv("15")).toBe(15)
    expect(stallMinutesFromEnv("7.5")).toBe(7.5)
    expect(stallMinutesFromEnv("0")).toBe(30) // сигнал на каждой паузе — мёртвый режим
    expect(stallMinutesFromEnv("-5")).toBe(30)
    expect(stallMinutesFromEnv("abc")).toBe(30)
    expect(stallMinutesFromEnv("NaN")).toBe(30)
  })
})

describe("lastActivityOf: активность последнего сообщения (список DESC)", () => {
  test("пустой список — сообщений нет (= застой)", () => {
    expect(lastActivityOf([])).toBeNull()
  })
  test("max по числовым полям времени (created/streamed/completed)", () => {
    expect(lastActivityOf([{ id: "m1", time: { created: 100, streamed: 200, completed: 300 } }])).toBe(300)
    expect(lastActivityOf([{ id: "m1", time: { created: 10, streamed: 90, completed: 50 } }])).toBe(90)
  })
  test("idle-маркер и сообщение без completed валидны — достаточно created", () => {
    expect(lastActivityOf([{ id: "m1", type: "assistant", time: { created: 123 } }])).toBe(123)
    expect(lastActivityOf([{ id: "m1", type: "idle", time: { created: 123, outcome: "success" } }])).toBe(123)
  })
  test("мусорное время — invalid (пропуск тика, не ложный сигнал)", () => {
    expect(lastActivityOf([{ id: "m1", time: {} }])).toBe("invalid")
    expect(lastActivityOf([{ id: "m1", time: null }])).toBe("invalid")
    expect(lastActivityOf([{ id: "m1" }])).toBe("invalid")
    expect(lastActivityOf([{ id: "m1", time: { created: "100" } }])).toBe("invalid")
    expect(lastActivityOf([null, { time: { created: 1 } }])).toBe("invalid")
  })
  test("берётся только первое сообщение — мусор в [0] не спасают старые", () => {
    expect(lastActivityOf([{ time: {} }, { time: { created: 999 } }])).toBe("invalid")
  })
})

describe("stallVerdict: зависший ход при живой сессии", () => {
  const NOW = 1_000_000_000_000
  const MIN = 60_000
  const facts = (over: Partial<StallFacts> = {}): StallFacts => ({
    status: "In Progress",
    sessionAlive: true,
    lastActivityAt: NOW - 40 * MIN,
    branch: "work/task-abc",
    branchInWorktree: true,
    signaled: false,
    now: NOW,
    ...over,
  })

  test("живой In Progress со свежей активностью — none", () => {
    expect(stallVerdict(facts({ lastActivityAt: NOW - 5 * MIN }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("завис 40 мин при пороге 30 — signal; текст несёт минуты и диагноз ветки", () => {
    const v = stallVerdict(facts(), 30 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("[factory]")
    expect(v.text).toContain("тишина 40 мин")
    expect(v.text).toContain("пороге 30 мин")
    expect(v.text).toContain("ветка на месте (work/task-abc)")
    expect(v.text).toContain("комментарий")
    // совет «что делать» называет все рабочие пути будилки: коммент, Design
    // (перепланировка), In Progress (fast-track «поправь» — будит return-to-dev)
    expect(v.text).toContain("Design")
    expect(v.text).toContain("In Progress")
    expect(v.text).not.toContain("переставь статус")
  })
  test("повторный тик после сигнала — silent (анти-спам раз за эпизод)", () => {
    expect(stallVerdict(facts({ signaled: true }), 30 * MIN)).toEqual({ t: "silent" })
  })
  test("активность вернулась при стоявшем флаге — none (демон снимет флаг)", () => {
    expect(stallVerdict(facts({ signaled: true, lastActivityAt: NOW - 2 * MIN }), 30 * MIN))
      .toEqual({ t: "none" })
  })
  test("не In Progress — none даже при застое", () => {
    expect(stallVerdict(facts({ status: "Needs attention" }), 30 * MIN)).toEqual({ t: "none" })
    expect(stallVerdict(facts({ status: "Design" }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("dead и unknown сессия — none (соседний код карантин / пропуск тика)", () => {
    expect(stallVerdict(facts({ sessionAlive: false }), 30 * MIN)).toEqual({ t: "none" })
    expect(stallVerdict(facts({ sessionAlive: null }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("сообщений нет вовсе — signal (трекнутая сессия минимум с промптом)", () => {
    const v = stallVerdict(facts({ lastActivityAt: null }), 30 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("сообщений в сессии нет")
  })
  test("диагноз ветки: пустая ячейка и ветка вне ворктри, значение обрезано до 60", () => {
    const empty = stallVerdict(facts({ branch: "" }), 30 * MIN)
    expect(empty.t).toBe("signal")
    if (empty.t !== "signal") return
    expect(empty.text).toContain("Branch-ячейка пуста")
    const lost = stallVerdict(facts({ branchInWorktree: false, branch: "work/task-xyz" }), 30 * MIN)
    expect(lost.t).toBe("signal")
    if (lost.t !== "signal") return
    expect(lost.text).toContain("ветки нет в ворктри (work/task-xyz)")
    const long = `work/${"b".repeat(80)}`
    const cut = stallVerdict(facts({ branchInWorktree: false, branch: long }), 30 * MIN)
    if (cut.t !== "signal") return
    expect(cut.text).toContain(long.slice(0, 60))
    expect(cut.text).not.toContain(long)
  })
  test("ровно на пороге — ещё none (возраст строго больше порога)", () => {
    expect(stallVerdict(facts({ lastActivityAt: NOW - 30 * MIN }), 30 * MIN)).toEqual({ t: "none" })
  })
})

describe("adoptBodyDoc: самоисцеление пустого bodyDoc трека", () => {
  const tracked = (bodyDoc: string): TrackedTask => ({
    sessionID: "ses_x", worktree: "", branch: "", bodyDoc,
    title: "Задача", lastCommentAt: 0, lastCommentId: "",
    createdAt: 1, attempts: 0, project: "t",
  })
  const row = (bodyDoc: string): TaskRef => ({
    rowId: "row1", bodyDoc, title: "Задача", status: "In Progress", pr: "", branch: "",
  })

  test("пустой трек + резолв строки — тело принято", () => {
    expect(adoptBodyDoc(tracked(""), row("20261105000000-bodydoc"))).toBe("20261105000000-bodydoc")
  })

  test("непустой трек не перезаписывается (идемпотентность)", () => {
    expect(adoptBodyDoc(tracked("20261105000000-old"), row("20261105000000-new"))).toBeNull()
  })

  test("строка без резолва — adopt не срабатывает", () => {
    expect(adoptBodyDoc(tracked(""), row(""))).toBeNull()
  })
})

// ——— тайтл → сегмент hpath (фоллбек тела detached-строк) ———

describe("pathSafe", () => {
  test("'/' заменяется на U+2215 — паритет CLI path_safe", () => {
    expect(pathSafe("вынести сборки из /tmp на /home")).toBe("вынести сборки из \u{2215}tmp на \u{2215}home")
  })

  test("тайтл без слэшей не меняется", () => {
    expect(pathSafe("обычный тайтл (v2)")).toBe("обычный тайтл (v2)")
  })

  test("уже санитизированный остаётся как есть (идемпотентность)", () => {
    expect(pathSafe("уже \u{2215} санитизирован")).toBe("уже \u{2215} санитизирован")
  })
})
