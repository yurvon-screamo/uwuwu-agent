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
  runBatch,
  parseProjects,
  formatPassSummary,
  adoptBodyDoc,
  pathSafe,
  createBoardPathOnce,
  errText,
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
  alertTargetOf,
  spawnGuardVerdict,
  spawnLimitsFromEnv,
  untrackedRowDecision,
  activeTaskGuards,
  SPAWN_ALERT_REPEAT_MS,
  type SpawnGuardLimits,
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
  test("envDir: строка с / валидна, чужие типы — мимо", () => {
    expect(isValidTracked({ ...base, envDir: "/home/x/agents-env/row" })).toBe(true)
    expect(isValidTracked({ ...base, envDir: undefined })).toBe(true)
    expect(isValidTracked({ ...base, envDir: "relative" })).toBe(false)
  })
  test("чужие поля legacy-хвоста перехода игнорируются — запись валидна", () => {
    // прод-state содержит записи со штампом legacy переходного периода:
    // после выпила штампа валидатор не должен ронять их как «битые»
    expect(isValidTracked({ ...base, legacy: true })).toBe(true)
  })
  test("mergedPrUrl: http(s)-строка и undefined валидны, не-url и чужие типы — мимо", () => {
    expect(isValidTracked({ ...base, mergedPrUrl: "https://github.com/o/r/pull/1" })).toBe(true)
    expect(isValidTracked({ ...base, mergedPrUrl: undefined })).toBe(true)
    expect(isValidTracked({ ...base, mergedPrUrl: "not-a-url" })).toBe(false)
    expect(isValidTracked({ ...base, mergedPrUrl: true })).toBe(false)
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
    // скан территории до плана (кейс vgcm1t5 ↔ PR #79: план, дублирующий чужой PR, мёртв при рождении)
    expect(p).toContain("скан территории")
    expect(p).toContain("task list")
    expect(p).toContain("gh pr list")
    // живые указатели доски (кейс сводной dzype6z: 5 из 6 row-id мертвы — план адресовал фантомы)
    expect(p).toContain("указатели постановки")
    expect(p).toContain("гниёт")
  })
  test("implement-режим (fast-track): без предписания план-ревью, сразу реализация → «Ревью результата»", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement")
    expect(p).toContain("In Progress")
    expect(p).toContain("пропускаешь")
    expect(p).toContain("«Ревью результата»")
    expect(p).toContain("task get")
    // предписание план-ревью («План утрясан → … «Ревью плана»») — только в design-режиме
    expect(p).not.toContain("План утрясан")
    // hint скана территории — тоже design-specific, в fast-track не протекает
    expect(p).not.toContain("скан территории")
    // живые указатели — тоже design-specific
    expect(p).not.toContain("указатели постановки")
    expect(p).not.toContain("🤖")
  })
  test("env-контекст: работа в готовом окружении, старой строки «создай worktree» нет", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement", {
      repoDir: "/home/x/agents-env/r1/repo", branch: "work/task-r1", scratchDir: "/home/x/agents-env/r1/scratch",
    })
    expect(p).toContain("/home/x/agents-env/r1/repo")
    expect(p).toContain("work/task-r1")
    expect(p).toContain("не /tmp")
    expect(p).toContain("заполняет фабрика")
    expect(p).not.toContain("создай worktree")
  })
  test("без env-контекста (env:false-проект) — прежняя строка про worktree+Branch-ячейку", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent")
    expect(p).toContain("создай worktree+ветку от origin/master")
    expect(p).toContain("заполни Branch-ячейку")
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
  test("env-контекст: окружение уже существует — своё не создавай, пути названы", () => {
    const p = continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", {
      repoDir: "/home/x/agents-env/r1/repo", branch: "work/task-r1", scratchDir: "/home/x/agents-env/r1/scratch",
    })
    expect(p).toContain("уже существует — своё не создавай")
    expect(p).toContain("/home/x/agents-env/r1/repo")
    expect(p).toContain("work/task-r1")
    expect(p).toContain("Незакоммиченное — там же")
  })
  test("без env-контекста (env:false-проект) — строки окружения нет", () => {
    const p = continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent")
    expect(p).not.toContain("своё не создавай")
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
  test("env-контекст назван, чтобы агент не плодил окружение заново", () => {
    const env = { repoDir: "/env/r", branch: "work/task-r1", scratchDir: "/env/s" }
    const p = conflictFixPrompt("r1", "https://github.com/x/pull/9", env)
    expect(p).toContain(env.repoDir)
    expect(p).toContain(env.branch)
    expect(p).toContain("своё не создавай")
    expect(conflictFixPrompt("r1", "https://github.com/x/pull/9")).not.toContain("своё не создавай")
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
  test("env прокидывается во все ветки (work-line/окружение названо)", () => {
    const env = { repoDir: "/env/r", branch: "work/task-r1", scratchDir: "/env/s" }
    expect(promptForStart("implement", ...args, env))
      .toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement", env))
    expect(promptForStart("implement", ...args, env)).toContain(env.repoDir)
    // рестарт env-задачи называет окружение — агент не плодит новое
    expect(promptForStart("continue", ...args, env))
      .toBe(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", env))
    expect(promptForStart("continue", ...args, env)).toContain(env.repoDir)
    expect(promptForStart("conflict-fix", ...args, env))
      .toBe(conflictFixPrompt("r1", "https://github.com/x/pull/9", env))
    expect(promptForStart("conflict-fix", ...args, env)).toContain(env.repoDir)
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
    // вердикт-строка с тремя исходами; approve-with-notes plan-у не светит
    expect(p).toContain("approve|approve с мелкими замечаниями|request_changes")
    expect(p).toContain("только для result")
    expect(p).not.toContain("approve с мелкими замечаниями (все находки Low) → In Progress")
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
  test("result: approve-with-notes → In Progress с пометкой дельты и прошлых раундов", () => {
    const p = reviewSpawnPrompt("r5", "uwuwu", "result", { branch: "", worktree: "", pr: "" })
    expect(p).toContain("approve с мелкими замечаниями (все находки Low) → In Progress")
    expect(p).toContain("следующее ревью с тем же ревьювером")
    expect(p).toContain("прошлые раунды ревью этой задачи")
  })
  test("result без ворктри (env:false / карантинный ревью-вход) — честная строка про diff/PR, не пустота", () => {
    const p = reviewSpawnPrompt("r3", "uwuwu", "result", { branch: "feat/y", worktree: "", pr: "" })
    expect(p).toContain("Ворктри задачи не найден")
    expect(p).toContain("diff origin/master…feat/y")
    expect(p).not.toContain("Ворктри задачи: ")
  })
  test("plan без ворктри — строка про diff не появляется (плану код не нужен)", () => {
    const p = reviewSpawnPrompt("r4", "uwuwu", "plan", { branch: "", worktree: "", pr: "" })
    expect(p).not.toContain("Ворктри задачи не найден")
  })
})

describe("reviewRerunPrompt / reviewCommentsPrompt / reviewReturnPrompt", () => {
  test("rerun: статус раунда + переходы + контекст ветки", () => {
    const p = reviewRerunPrompt("r1", "result", { branch: "b", worktree: "/w", pr: "https://pr/1" })
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("устранение своих прошлых замечаний")
    expect(p).toContain("approve → Review")
    expect(p).toContain("request_changes → In Progress")
    expect(p).toContain("https://pr/1")
  })
  test("rerun result: дельта-ветка approve-with-notes + эскалация повторных мелочей", () => {
    const p = reviewRerunPrompt("r2", "result", { branch: "", worktree: "", pr: "" })
    expect(p).toContain("approve с мелкими замечаниями (все находки Low) → In Progress")
    expect(p).toContain("дельта-раунд")
    expect(p).toContain("мелочи повторно не устранены")
  })
  test("rerun plan: дельта-ветка result не появляется, переходы плановые", () => {
    const p = reviewRerunPrompt("r3", "plan", { branch: "", worktree: "", pr: "" })
    expect(p).toContain("approve → In Progress")
    expect(p).toContain("request_changes → Design")
    expect(p).not.toContain("дельта-раунд")
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
  test("return: approve-with-notes → дофикси и сразу «Ревью результата», чистый approve — мяч юзеру", () => {
    const p = reviewReturnPrompt("r4")
    expect(p).toContain("approve с мелкими замечаниями")
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("дельта")
    expect(p).toContain("чистый")
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

  test("env-флаг: boolean проходит и сохраняется, не-boolean — отказ", () => {
    const raw = JSON.stringify({ test: { board: "b", repo: "r", local: "/l", env: false } })
    expect(parseProjects(raw, fallback).test?.env).toBe(false)
    expect(parseProjects(undefined, fallback).uwuwu?.env).toBeUndefined()
    const bad = JSON.stringify({ test: { board: "b", repo: "r", local: "/l", env: "no" } })
    expect(() => parseProjects(bad, fallback)).toThrow(/test.*env.*boolean/)
  })
})

// ——— сводка прохода ———

describe("formatPassSummary: одна строка о проходе", () => {
  test("полная сводка с фейлами перечисляет причины", () => {
    const line = formatPassSummary("uwuwu", {
      devSpawned: 4, reviewSpawned: 1, wakeups: 2, cleanups: 1,
      envCreated: 2, envRemoved: 1, envAtticked: 1, watched: 12,
      failures: ["20261105ab: сессия мертва после спавна"],
    })
    expect(line).toBe(
      "проход uwuwu: спавн dev=4 ревью=1, будильники=2, чистки=1, env=+2/-1/attic:1, наблюдение=12" +
      ", фейлы=1 (20261105ab: сессия мертва после спавна)")
  })

  test("тихий тик без событий всё равно виден (наблюдение=0)", () => {
    const line = formatPassSummary("origa", {
      devSpawned: 0, reviewSpawned: 0, wakeups: 0, cleanups: 0,
      envCreated: 0, envRemoved: 0, envAtticked: 0,
      watched: 0, failures: [],
    })
    expect(line).toBe("проход origa: спавн dev=0 ревью=0, будильники=0, чистки=0, env=+0/-0/attic:0, наблюдение=0")
  })

  test("задержанные гардом спавны видны в сводке", () => {
    const line = formatPassSummary("uwuwu", {
      devSpawned: 0, reviewSpawned: 0, wakeups: 0, cleanups: 0,
      envCreated: 0, envRemoved: 0, envAtticked: 0,
      watched: 3, failures: [], held: 15, heldReason: "state-loss",
    })
    expect(line).toContain("задержано гардом=15")
    expect(line).toContain("state-loss")
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
    prMerged: false,
    signaled: false,
    now: NOW,
    ...over,
  })

  test("живой In Progress со свежей активностью — none", () => {
    expect(stallVerdict(facts({ lastActivityAt: NOW - 5 * MIN }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("PR смержен при застое — none (тишина = штатное ожидание приёмки, прод-кейс 06.11)", () => {
    expect(stallVerdict(facts({ prMerged: true }), 30 * MIN)).toEqual({ t: "none" })
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

// ——— мемоизер пути доски на проход (фоллбек тела detached-строк) ———

describe("createBoardPathOnce — ленивый мемоизер пути доски на проход", () => {
  const logs: string[] = []
  const log = (m: string): void => { logs.push(m) }

  test("успех: путь кэшируется — один load на несколько вызовов", async () => {
    // Arrange
    logs.length = 0
    let loads = 0
    const load = async (): Promise<{ box?: string; hpath?: string }> => {
      loads++
      return { box: "nb", hpath: "/proj/Задачи x" }
    }

    // Act
    const resolve = createBoardPathOnce(load, log)
    const [a, b, c] = [await resolve(), await resolve(), await resolve()]

    // Assert: кэш отдаёт один и тот же активный путь, load и лог — однократно
    expect(a).toEqual({ kind: "active", box: "nb", hpath: "/proj/Задачи x" })
    expect(b).toEqual(a)
    expect(c).toEqual(a)
    expect(loads).toBe(1)
    expect(logs).toEqual([])
  })

  test("сбой get_doc: мемоизируется как failed — один load и один лог на проход", async () => {
    // Arrange
    logs.length = 0
    let loads = 0
    const load = async (): Promise<{ box?: string; hpath?: string }> => {
      loads++
      throw new Error("ядро лежит")
    }

    // Act
    const resolve = createBoardPathOnce(load, log)
    const results = [await resolve(), await resolve(), await resolve()]

    // Assert: все вызовы получили один failed без повторных load,
    // деградация — ровно одна строка лога (не per-row спам)
    for (const r of results) expect(r).toEqual({ kind: "failed", reason: "ядро лежит" })
    expect(loads).toBe(1)
    expect(logs).toHaveLength(1)
    expect(logs[0]).toContain("отключён на проход")
    expect(logs[0]).toContain("ядро лежит")
  })

  test("пустые box/hpath (SQL-лаг): disabled + один лог", async () => {
    // Arrange
    logs.length = 0
    let loads = 0
    const load = async (): Promise<{ box?: string; hpath?: string }> => {
      loads++
      return { box: "", hpath: "" }
    }

    // Act
    const resolve = createBoardPathOnce(load, log)
    const results = [await resolve(), await resolve()]

    // Assert
    for (const r of results) expect(r).toEqual({ kind: "disabled" })
    expect(loads).toBe(1)
    expect(logs).toHaveLength(1)
    expect(logs[0]).toContain("SQL-лаг")
  })

  test("errText: Error даёт message, не-Error — строку", () => {
    expect(errText(new Error("boom"))).toBe("boom")
    expect(errText("строка")).toBe("строка")
    expect(errText(42)).toBe("42")
  })
})

// ——— гард спавн-волны: рубеж каскада при потере/порче state (инцидент 06.11) ———

const ref = (overrides: Partial<TaskRef> & { rowId: string }): TaskRef => ({
  bodyDoc: "", title: "Задача", status: "Design", pr: "", branch: "",
  devSession: "", reviewSession: "", // явная нормализация: undefined тестируется отдельным кейсом
  ...overrides,
})

const GUARD_LIMITS: SpawnGuardLimits = { lossMax: 2, burstMax: 12 }
const GUARD_NOW = 1_000_000_000_000

describe("spawnLimitsFromEnv: UWUWU_FACTORY_SPAWN_LOSS_MAX / UWUWU_FACTORY_SPAWN_BURST_MAX", () => {
  test("таблица: дефолты 2/12 на пустоту/мусор/0/минус, валидные числа — они", () => {
    // Arrange / Act / Assert
    expect(spawnLimitsFromEnv(undefined, undefined)).toEqual({ lossMax: 2, burstMax: 12 })
    expect(spawnLimitsFromEnv("", "   ")).toEqual({ lossMax: 2, burstMax: 12 })
    expect(spawnLimitsFromEnv("abc", "-3")).toEqual({ lossMax: 2, burstMax: 12 })
    expect(spawnLimitsFromEnv("0", "0")).toEqual({ lossMax: 2, burstMax: 12 })
    expect(spawnLimitsFromEnv("1", "20")).toEqual({ lossMax: 1, burstMax: 20 })
    expect(spawnLimitsFromEnv("2.5", "12")).toEqual({ lossMax: 2.5, burstMax: 12 })
  })
})

describe("spawnGuardVerdict: потеря state не превращается в каскад спавнов", () => {
  test("штатный тик: сирот нет, untracked в пределах burst — allow", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 0, totalUntracked: 3, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toEqual({ t: "allow" })
  })

  test("легитимная волна новых задач без сессионных ячеек в пределах burst — allow", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 0, totalUntracked: 12, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toEqual({ t: "allow" })
  })

  test("легитимное опустошение (все задачи Done) — allow без алерта", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 0, totalUntracked: 0, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toEqual({ t: "allow" })
  })

  test("инцидент-сигнатура: state пуст, на доске живые строки с ячейками — hold(state-loss), alert=first", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 15, totalUntracked: 15, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict.t).toBe("hold")
    if (verdict.t !== "hold") return
    expect(verdict.reason).toBe("state-loss")
    expect(verdict.alert).toBe("first")
    expect(verdict.text).toContain("15")
    expect(verdict.text).toContain("state")
  })

  test("сироты в пределах lossMax, но всего untracked > burst — hold(burst)", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 1, totalUntracked: 20, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toMatchObject({ t: "hold", reason: "burst", alert: "first" })
  })

  test("чистый burst без сирот — hold(burst), alert=first", () => {
    // Act
    const verdict = spawnGuardVerdict({ orphaned: 0, totalUntracked: 20, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toMatchObject({ t: "hold", reason: "burst", alert: "first" })
  })

  test("повторный тик эпизода в пределах окна повтора — alert=none", () => {
    // Arrange: алерт был 10 минут назад (окно — час)
    const signaled = new Date(GUARD_NOW - 10 * 60_000).toISOString()

    // Act
    const verdict = spawnGuardVerdict(
      { orphaned: 15, totalUntracked: 15, holdSignaled: signaled, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toMatchObject({ t: "hold", reason: "state-loss", alert: "none" })
  })

  test("длящийся HOLD дольше окна повтора — alert=repeat", () => {
    // Arrange: алерт был 2 часа назад
    const signaled = new Date(GUARD_NOW - SPAWN_ALERT_REPEAT_MS - 60_000).toISOString()

    // Act
    const verdict = spawnGuardVerdict(
      { orphaned: 15, totalUntracked: 15, holdSignaled: signaled, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toMatchObject({ t: "hold", alert: "repeat" })
  })

  test("allow при стоящем флаге — вердикт allow (демон снимет флаг)", () => {
    // Arrange
    const signaled = new Date(GUARD_NOW - 60_000).toISOString()

    // Act
    const verdict = spawnGuardVerdict(
      { orphaned: 0, totalUntracked: 0, holdSignaled: signaled, now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toEqual({ t: "allow" })
  })

  test("битый holdSignaled — трактуется как отсутствие флага (alert=first)", () => {
    // Act
    const verdict = spawnGuardVerdict(
      { orphaned: 15, totalUntracked: 15, holdSignaled: "не-ISO", now: GUARD_NOW }, GUARD_LIMITS)

    // Assert
    expect(verdict).toMatchObject({ t: "hold", alert: "first" })
  })

  test("ровно на порогах — allow (триггер — строго больше)", () => {
    // Act / Assert
    expect(spawnGuardVerdict({ orphaned: 2, totalUntracked: 12, now: GUARD_NOW }, GUARD_LIMITS))
      .toEqual({ t: "allow" })
    expect(spawnGuardVerdict({ orphaned: 3, totalUntracked: 12, now: GUARD_NOW }, GUARD_LIMITS))
      .toMatchObject({ t: "hold", reason: "state-loss" })
    expect(spawnGuardVerdict({ orphaned: 0, totalUntracked: 13, now: GUARD_NOW }, GUARD_LIMITS))
      .toMatchObject({ t: "hold", reason: "burst" })
  })
})

describe("alertTargetOf: фолбэк-цепочка адресата алерта", () => {
  test("первая сирота с телом в порядке скана выигрывает", () => {
    // Arrange
    const candidates = [
      ref({ rowId: "r1", bodyDoc: "", devSession: "ses_a" }),
      ref({ rowId: "r2", bodyDoc: "doc2", devSession: "ses_b" }),
      ref({ rowId: "r3", bodyDoc: "doc3" }),
    ]

    // Act
    const target = alertTargetOf(candidates)

    // Assert
    expect(target?.rowId).toBe("r2")
  })

  test("сироты по ревью-ячейке тоже считаются", () => {
    // Arrange
    const candidates = [ref({ rowId: "r1", bodyDoc: "doc1", reviewSession: "ses_r" })]

    // Act
    const target = alertTargetOf(candidates)

    // Assert
    expect(target?.rowId).toBe("r1")
  })

  test("сироты без тел пропускаются — первая строка с телом", () => {
    // Arrange
    const candidates = [
      ref({ rowId: "r1", bodyDoc: "", devSession: "ses_a" }),
      ref({ rowId: "r2", bodyDoc: "doc2" }),
    ]

    // Act
    const target = alertTargetOf(candidates)

    // Assert
    expect(target?.rowId).toBe("r2")
  })

  test("чистый burst без сирот — первая untracked с телом", () => {
    // Arrange
    const candidates = [
      ref({ rowId: "r1", bodyDoc: "" }),
      ref({ rowId: "r2", bodyDoc: "doc2" }),
      ref({ rowId: "r3", bodyDoc: "doc3" }),
    ]

    // Act
    const target = alertTargetOf(candidates)

    // Assert
    expect(target?.rowId).toBe("r2")
  })

  test("ни одной строки с телом — null (лог-only)", () => {
    // Arrange
    const candidates = [ref({ rowId: "r1", bodyDoc: "" }), ref({ rowId: "r2", bodyDoc: "" })]

    // Act
    const target = alertTargetOf(candidates)

    // Assert
    expect(target).toBeNull()
  })

  test("undefined-поля ячеек — свидетельством не являются (TaskRef без нормализации)", () => {
    // Arrange: r1 несёт devSession, r2/r3 — поля не заданы вовсе
    const candidates = [
      ref({ rowId: "r1", bodyDoc: "doc1" }),
      ref({ rowId: "r2", bodyDoc: "doc2", devSession: undefined, reviewSession: undefined }),
    ]

    // Act
    const target = alertTargetOf(candidates)

    // Assert: сирот нет (undefined не свидетель) — фолбэк: первая с телом
    expect(target?.rowId).toBe("r1")
  })

  test("пустой список — null", () => {
    expect(alertTargetOf([])).toBeNull()
  })
})

describe("untrackedRowDecision: вердикт untracked строки (матрица плана 20261105174319)", () => {
  const cell = (devSession?: string, reviewSession?: string): Pick<TaskRef, "devSession" | "reviewSession"> => ({
    devSession,
    reviewSession,
  })

  test("чистая fresh-строка без свидетельств — спавн обычного пути, не сирота", () => {
    expect(untrackedRowDecision(cell(undefined, undefined), "alive")).toEqual({ devWave: true, orphan: false })
    expect(untrackedRowDecision(cell("", ""), "dead")).toEqual({ devWave: true, orphan: false })
  })

  test("ревью-свидетельство без трека (дев пуста) — сирота как прежде, защита инцидента 06.11", () => {
    expect(untrackedRowDecision(cell(undefined, "ses_rev01"), "dead")).toEqual({ devWave: true, orphan: true })
    expect(untrackedRowDecision(cell("", "ses_rev01"), "alive")).toEqual({ devWave: true, orphan: true })
  })

  test("живая дев-заявка (task claim) — внешняя работа идёт: не спавнить, не сирота", () => {
    expect(untrackedRowDecision(cell("ses_dev01", undefined), "alive")).toEqual({ devWave: false, orphan: false })
  })

  test("liveness-проба не удалась — трактуем как живую (прецедент routeTask)", () => {
    expect(untrackedRowDecision(cell("ses_dev01", undefined), "unknown")).toEqual({ devWave: false, orphan: false })
  })

  test("мёртвая дев-заявка — сирота как прежде и остаётся в дев-волне (ре-спавн при allow)", () => {
    // «как прежде» = сирота в подсчёте гарда + спавн при allow: путь
    // автокомпенсации lossMax-допуска (1-2 сироты спавнятся, ≥3 — HOLD)
    expect(untrackedRowDecision(cell("ses_dev01", undefined), "dead")).toEqual({ devWave: true, orphan: true })
  })

  test("мёртвый claim с ревью-свидетельством — сирота (любое свидетельство)", () => {
    expect(untrackedRowDecision(cell("ses_dev01", "ses_rev01"), "dead")).toEqual({ devWave: true, orphan: true })
  })
})

describe("activeTaskGuards", () => {
  const tracked = (branch: string, worktree: string, envDir?: string) => ({
    sessionID: "ses_abc", worktree, branch,
    bodyDoc: "d1", title: "t", project: "uwuwu", lastCommentAt: 1, lastCommentId: "i", createdAt: 2, attempts: 0,
    ...(envDir !== undefined ? { envDir } : {}),
  })
  const state = (tasks: Record<string, ReturnType<typeof tracked>>): FactoryState => ({ tasks })

  test("ветки и каталоги (worktree + envDir) активных задач собираются", () => {
    const guards = activeTaskGuards(state({
      a: tracked("work/task-a", "/env/a/repo", "/env/a"),
      b: tracked("factory/legacy-branch", "/wt/legacy"),
    }))
    expect([...guards.branches].sort()).toEqual(["factory/legacy-branch", "work/task-a"])
    expect([...guards.dirs].sort()).toEqual(["/env/a", "/env/a/repo", "/wt/legacy"])
  })

  test("пустые ветка/ворктри (карантин) — мимо наборов", () => {
    const guards = activeTaskGuards(state({ c: tracked("", "",) }))
    expect(guards.branches.size).toBe(0)
    expect(guards.dirs.size).toBe(0)
  })

  test("пустой state — пустые наборы", () => {
    const guards = activeTaskGuards({ tasks: {} })
    expect(guards.branches.size).toBe(0)
    expect(guards.dirs.size).toBe(0)
  })
})
