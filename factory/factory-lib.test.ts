// factory-lib.test.ts — юнит-тесты чистых функций uwuwu-factory.
import { describe, expect, test } from "bun:test"
import {
  commentText,
  conflictFixPrompt,
  continuePrompt,
  kickoffModeOf,
  baseOf,
  DEFAULT_BASE,
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
  reviewStallVerdict,
  routeTask,
  sessionSlotOf,
  TESTING_STATUS,
  testingSpawnPrompt,
  testingRerunPrompt,
  testingCommentsPrompt,
  REVIEW_PLAN_STATUS,
  REVIEW_RESULT_STATUS,
  USER_REVIEW_STATUS,
  stallMinutesFromEnv,
  lastActivityOf,
  stallVerdict,
  stallClock,
  bodyEditRoute,
  prProbeVerdict,
  PR_PROBE_UNKNOWN_MAX,
  type FactoryState,
  type RouteFacts,
  type ReviewStallFacts,
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
  stripAgentHead,
  parseAgentHeadTs,
  freshForeignComments,
  legacyRebaselineNeeded,
  latestComment,
  bodyContentHash,
  bodyLastmodMs,
  wakeupDedup,
  spawnInitBlock,
  stripExportWrapper,
  stripFrontmatter,
  resolvePromptWorktree,
  type SpawnInit,
} from "./factory-lib"
import { parseCommentsMarkdown } from "../lib/platform"
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

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
      { rowId: "r1", bodyDoc: "d1", title: "a", status: "Todo", type: "", pr: "", branch: "" },
      { rowId: "r2", bodyDoc: "d2", title: "b", status: "Todo", type: "", pr: "", branch: "" },
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
  test("reviewKind: вид ревью и отсутствие (legacy) валидны, чужие строки — мимо", () => {
    expect(isValidTracked({ ...base, reviewKind: "plan" })).toBe(true)
    expect(isValidTracked({ ...base, reviewKind: "result" })).toBe(true)
    expect(isValidTracked({ ...base, reviewKind: undefined })).toBe(true)
    expect(isValidTracked({ ...base, reviewKind: "full" })).toBe(false)
    expect(isValidTracked({ ...base, reviewKind: "" })).toBe(false)
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
    expect(p).toContain("task list --project uwuwu")
    expect(p).toContain("gh pr list")
    // живые указатели доски (кейс сводной dzype6z: 5 из 6 row-id мертвы — план адресовал фантомы)
    expect(p).toContain("указатели постановки")
    expect(p).toContain("гниёт")
    // командные примеры с id и проектом (инцидент 05.11: вопросный канал без
    // флага не приземлился — спавн знает оба)
    expect(p).toContain("task get r1 --project uwuwu")
    expect(p).toContain("task comment r1 --project uwuwu")
    expect(p).toContain("task edit r1 --project uwuwu --status \"Needs attention\"")
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
      env: {
        repoDir: "/home/x/agents-env/r1/repo", branch: "work/task-r1", scratchDir: "/home/x/agents-env/r1/scratch",
      },
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
  test("env:false с envRoot — канон-путь ворктри назван (тот же, что резолвит tester/ревьювер)", () => {
    const p = spawnPrompt("r1", "true-washing", "true-washing", "implement", { envRoot: "/home/x/agents-env" })
    expect(p).toContain("ворктри /home/x/agents-env/r1/repo")
    expect(p).toContain("скретч /home/x/agents-env/r1/scratch")
    expect(p).toContain("заполни Branch-ячейку")
  })
  test("формула пути одна: промпт sanitize row-id так же, как резолв tester/ревьювера", () => {
    // патологический id из тест-кейса sanitizeRowId: ведущий дефис срезается —
    // сырой rowId в промпте отправил бы агента создавать ворктри по пути,
    // который resolvePromptWorktree никогда не пробует (ревью PR #192, Low)
    const id = "-5BF9NMBXZ"
    const p = spawnPrompt(id, "true-washing", "true-washing", "implement", { envRoot: "/env" })
    expect(p).toContain("ворктри /env/5BF9NMBXZ/repo")
    expect(p).not.toContain("/env/-5BF9NMBXZ/repo")
  })
  test("base из контекста: env:false-строка и env:true-строка говорят базу проекта, не дефолт", () => {
    const p = spawnPrompt("r1", "1xgames", "1xgames/game-service", "implement", { base: "origin/main" })
    expect(p).toContain("создай worktree+ветку от origin/main")
    const withEnv = spawnPrompt("r2", "1xgames", "1xgames/game-service", "implement", {
      base: "origin/main",
      env: { repoDir: "/env/r2/repo", branch: "work/task-r2", scratchDir: "/env/r2/scratch" },
    })
    expect(withEnv).toContain("ветка уже от origin/main")
  })
  test("docs-only финал назван: без PR/CI, PR-ячейка = вики-артефакт (кейс 20261104094441-wvv9psh)", () => {
    const p = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement")
    expect(p).toContain("docs-only — без PR/CI")
    expect(p).toContain("URL главного")
    expect(p).toContain("закрытие без мержа")
    // PR-путь остаётся каноном кодовых задач
    expect(p).toContain("gh pr create")
  })
})

describe("continuePrompt", () => {
  test("рестарт без предписания стадии, с якорем на пайп AGENTS.md", () => {
    const p = continuePrompt("r1", "origa", "yurvon-screamo/origa")
    expect(p).toContain("Задача r1")
    expect(p).toContain("перезапущена")
    expect(p).toContain("AGENTS.md")
    expect(p).toContain("task get")
    // командные примеры с id и проектом — тот же контракт вопросного канала
    expect(p).toContain("task get r1 --project origa")
    expect(p).toContain("task comment r1 --project origa")
    expect(p).toContain("task edit r1 --project origa --status \"Needs attention\"")
    expect(p).not.toContain("Стадия Design")
    expect(p).not.toContain("«Ревью плана»")
    expect(p).not.toContain("🤖")
  })
  test("env-контекст: окружение уже существует — своё не создавай, пути названы", () => {
    const p = continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", {
      env: {
        repoDir: "/home/x/agents-env/r1/repo", branch: "work/task-r1", scratchDir: "/home/x/agents-env/r1/scratch",
      },
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
  test("контракт: перечитай тело, определи чью правку, эхо — тихий конец", () => {
    const p = bodyEditPrompt("r1")
    expect(p).toContain("Тело задачи r1 изменено")
    expect(p).toContain("task get")
    expect(p).toContain("чью это правку")
    expect(p).toContain("эхо")
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
    testerAlive: null,
    freshComments: [],
    ...over,
  })
  test("Вейт-факты — фабрика немая: крон-мастера и ран-карточки ей невидимы", () => {
    // крон-мастер в Вейт (файр его — работа планировщика, не фабрики)
    expect(routeTask(facts({ status: "Вейт", lastStatus: "Todo" }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "Вейт", lastStatus: "" }))).toEqual({ t: "noop" })
    // ран-карточка в Todo (создана планировщиком) — тоже не роутится
    expect(routeTask(facts({ status: "Todo", lastStatus: "" }))).toEqual({ t: "noop" })
    // ран из Вейт переброшен владельцем в In Progress — штатный fast-track
    // «вмешайся и почини» (возврат девелоперу, спавн фильтром spawnable)
    const live = routeTask(facts({ status: "In Progress", lastStatus: "Вейт", reviewerAlive: false }))
    expect(live).toEqual({ t: "return-to-dev" })
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
  test("живая сессия того же вида + вход/смена вида → доревью", () => {
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: true, reviewKind: "plan",
    }))).toEqual({ t: "rerun-reviewer", kind: "plan" })
    // повторный раунд фиксов: вышел и вернулся в тот же вид
    expect(routeTask(facts({
      status: REVIEW_RESULT_STATUS, lastStatus: "In Progress", reviewerAlive: true, reviewKind: "result",
    }))).toEqual({ t: "rerun-reviewer", kind: "result" })
  })
  test("живая сессия другого вида → спавн нового держателя", () => {
    // штатный переход план → результат (через In Progress — lastStatus вид не различает)
    expect(routeTask(facts({
      status: REVIEW_RESULT_STATUS, lastStatus: "In Progress", reviewerAlive: true, reviewKind: "plan",
    }))).toEqual({ t: "spawn-reviewer", kind: "result" })
    // ручная прямая перестановка план → результат без промежуточного статуса
    expect(routeTask(facts({
      status: REVIEW_RESULT_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true, reviewKind: "plan",
    }))).toEqual({ t: "spawn-reviewer", kind: "result" })
    // обратная смена: живой держатель результата, статус плана
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: REVIEW_RESULT_STATUS, reviewerAlive: true, reviewKind: "result",
    }))).toEqual({ t: "spawn-reviewer", kind: "plan" })
  })
  test("живая legacy-сессия без вида → спавн нового держателя (миграция на два агента)", () => {
    expect(routeTask(facts({
      status: REVIEW_RESULT_STATUS, lastStatus: REVIEW_RESULT_STATUS, reviewerAlive: true,
    }))).toEqual({ t: "spawn-reviewer", kind: "result" })
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: true,
    }))).toEqual({ t: "spawn-reviewer", kind: "plan" })
  })
  test("живая сессия того же вида в том же статусе: комменты юзера → ревьюверу, тишина → noop", () => {
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true, reviewKind: "plan",
      freshComments: [c("x", "вопрос", 1)],
    }))).toEqual({ t: "comments-to-reviewer" })
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: REVIEW_PLAN_STATUS, reviewerAlive: true, reviewKind: "plan",
    }))).toEqual({ t: "noop" })
  })
  test("liveness неизвестна → глухой noop: ни спавна, ни дельты сессии неизвестного держателя", () => {
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: null, reviewKind: "plan",
    }))).toEqual({ t: "noop" })
    expect(routeTask(facts({
      status: REVIEW_PLAN_STATUS, lastStatus: "Design", reviewerAlive: null,
    }))).toEqual({ t: "noop" })
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
    const p = conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu")
    expect(p).toContain("r1")
    expect(p).toContain("проект uwuwu")
    expect(p).toContain("https://github.com/x/pull/9")
    expect(p).toContain("конфликт")
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("mergeable")
    expect(p).not.toContain("Стадия Design")
    expect(p).not.toContain("Ревью плана")
  })
  test("env-контекст назван, чтобы агент не плодил окружение заново", () => {
    const env = { repoDir: "/env/r", branch: "work/task-r1", scratchDir: "/env/s" }
    const p = conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu", { env })
    expect(p).toContain(env.repoDir)
    expect(p).toContain(env.branch)
    expect(p).toContain("своё не создавай")
    expect(conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu")).not.toContain("своё не создавай")
  })
  test("живая сессия (будильник мимо startTask) — без инит-блока: дельта-принцип", () => {
    const p = conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu")
    expect(p).not.toContain("Инит-данные")
  })
  test("base из контекста: конфликт и починка называют базу проекта, не master", () => {
    const p = conflictFixPrompt("r1", "https://git.l4a.ru/x/!39", "1xgames", { base: "origin/main" })
    expect(p).toContain("конфликтует с origin/main")
    expect(p).toContain("обнови ветку от origin/main")
  })
})

describe("promptForStart: единственная точка выбора стартового промпта", () => {
  const target = {
    rowId: "r1", project: "uwuwu", repo: "yurvon-screamo/uwuwu-agent", pr: "https://github.com/x/pull/9",
  }
  test("conflict-fix → конфликт-kickoff", () => {
    const p = promptForStart("conflict-fix", target)
    expect(p).toBe(conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu"))
  })
  test("continue → рестарт без предписания стадии", () => {
    const p = promptForStart("continue", target)
    expect(p).toBe(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent"))
    expect(p).not.toContain("Стадия Design")
  })
  test("implement/design → spawnPrompt соответствующего режима", () => {
    expect(promptForStart("implement", target)).toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement"))
    expect(promptForStart("design", target)).toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "design"))
  })
  test("env прокидывается во все ветки (work-line/окружение названо)", () => {
    const env = { repoDir: "/env/r", branch: "work/task-r1", scratchDir: "/env/s" }
    expect(promptForStart("implement", target, { env }))
      .toBe(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "implement", { env }))
    expect(promptForStart("implement", target, { env })).toContain(env.repoDir)
    // рестарт env-задачи называет окружение — агент не плодит новое
    expect(promptForStart("continue", target, { env }))
      .toBe(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", { env }))
    expect(promptForStart("continue", target, { env })).toContain(env.repoDir)
    expect(promptForStart("conflict-fix", target, { env }))
      .toBe(conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu", { env }))
    expect(promptForStart("conflict-fix", target, { env })).toContain(env.repoDir)
  })
  test("init прокидывается во все ветки — все три вида старта создают свежую сессию", () => {
    const init: SpawnInit = {
      person: null, projectDocs: [],
      task: { title: "Т", status: "Design", type: "", branch: "", pr: "" },
    }
    expect(promptForStart("implement", target, { init })).toContain("Инит-данные")
    expect(promptForStart("continue", target, { init })).toContain("Инит-данные")
    expect(promptForStart("conflict-fix", target, { init })).toContain("Инит-данные")
  })
})

describe("stripFrontmatter/stripExportWrapper: паритет CLI markdown.rs", () => {
  test("frontmatter срезан, тело без потерь", () => {
    expect(stripFrontmatter("---\ntitle: X\n---\n\nтело\n")).toBe("тело\n")
  })
  test("без frontmatter — дословно; незакрытый fence — дословно", () => {
    expect(stripFrontmatter("тело\n")).toBe("тело\n")
    expect(stripFrontmatter("---\ntitle: X\nтело\n")).toBe("---\ntitle: X\nтело\n")
  })
  test("обёртка экспорта: frontmatter + строка тайтла — долой", () => {
    expect(stripExportWrapper("---\ntitle: таск\n---\n\n# таск\n\nтело\n", "таск")).toBe("тело\n")
  })
  test("чужой H1 в начале не срезается (не тайтл дока)", () => {
    expect(stripExportWrapper("# другой заголовок\nтекст\n", "таск")).toBe("# другой заголовок\nтекст\n")
  })
})

describe("spawnInitBlock: инит-данные первого промпта", () => {
  const env = { repoDir: "/env/r", branch: "work/task-r1", scratchDir: "/env/s" }
  const baseInit: SpawnInit = {
    person: "Язык — русский.\nАнти-паттерн: не дробить задачи.",
    projectDocs: ["uwuwu/README", "uwuwu/Домены"],
    task: { title: "Фича", status: "Design", type: "dev", branch: "work/task-r1", pr: "" },
  }
  test("строка задачи: тайтл/статус/тип/ячейки, пустые — тире", () => {
    const b = spawnInitBlock(baseInit, "r1", "uwuwu")
    expect(b).toContain("Задача: «Фича», статус Design, тип dev, ветка work/task-r1, PR —.")
    expect(b).toContain('uwuwu-cli task get r1 --project uwuwu')
  })
  test("легаси-строка без типа — тире в слоте типа", () => {
    const b = spawnInitBlock({ ...baseInit, task: { ...baseInit.task, type: "" } }, "r1", "uwuwu")
    expect(b).toContain("Задача: «Фича», статус Design, тип —, ветка work/task-r1, PR —.")
  })
  test("проект: слаги живых доков как готовые uwuwu-cli read", () => {
    const b = spawnInitBlock(baseInit, "r1", "uwuwu")
    expect(b).toContain('uwuwu-cli read "uwuwu/README", "uwuwu/Домены"')
    expect(b).not.toContain("чекаут")
  })
  test("проект без живых доков — честная строка вместо заглушек", () => {
    const b = spawnInitBlock({ ...baseInit, projectDocs: [] }, "r1", "uwuwu")
    expect(b).toContain("канон-доки (README/Домены/Стек/Деплоймент) не нашлись")
    expect(b).not.toContain("uwuwu-cli read")
  })
  test("env:false-проект: чекаут назван с маркером env:false", () => {
    const b = spawnInitBlock({ ...baseInit, localPath: "/srv/origa" }, "r1", "uwuwu")
    expect(b).toContain("чекаут /srv/origa (env:false — ворктри/ветка сам)")
  })
  test("персона: целиком с отступом, помечена как язык/стиль/анти-паттерны", () => {
    const b = spawnInitBlock(baseInit, "r1", "uwuwu")
    expect(b).toContain("Персона владельца (язык/стиль/анти-паттерны):")
    expect(b).toContain("  Язык — русский.")
    expect(b).toContain("  Анти-паттерн: не дробить задачи.")
  })
  test("персона недоступна — честная строка, спавн не зависит от инита", () => {
    const b = spawnInitBlock({ ...baseInit, person: null }, "r1", "uwuwu")
    expect(b).toContain("Персона владельца: недоступна")
    expect(b).toContain("работай без неё")
  })
  test("шапка блока несёт конвенцию «первичный источник»", () => {
    const b = spawnInitBlock(baseInit, "r1", "uwuwu")
    expect(b).toContain("первичный источник ориентации")
    expect(b).toContain("глубокое чтение — по необходимости")
  })
  test("spawnPrompt с init: блок после шапки, до инструкций; без init — блока нет", () => {
    const withInit = spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", "design", { env, init: baseInit })
    const header = withInit.indexOf("Задача r1")
    const block = withInit.indexOf("Инит-данные")
    const instructions = withInit.indexOf("Работай по AGENTS.md")
    expect(block).toBeGreaterThan(header)
    expect(block).toBeLessThan(instructions)
    expect(withInit).toContain("Персона владельца")
    expect(spawnPrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent")).not.toContain("Инит-данные")
    expect(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent")).not.toContain("Инит-данные")
    expect(continuePrompt("r1", "uwuwu", "yurvon-screamo/uwuwu-agent", { init: baseInit })).toContain("Инит-данные")
    expect(conflictFixPrompt("r1", "https://github.com/x/pull/9", "uwuwu", { init: baseInit })).toContain("Инит-данные")
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
    const p = reviewSpawnPrompt("r1", "uwuwu", "plan", { branch: "", worktree: "", pr: "" }, "plan-reviewer")
    expect(p).toContain("ревью плана")
    // методичка адресуется по имени праймари-агента держателя
    expect(p).toContain("праймари-агент plan-reviewer")
    expect(p).toContain("uwuwu-cli task get r1 --project uwuwu")
    expect(p).toContain("approve → In Progress")
    expect(p).toContain("request_changes → Design")
    expect(p).not.toContain("request_changes → In Progress")
    // вердикт-строка план-ревью двухзначная — словарь методички plan-reviewer
    expect(p).toContain("Статус: approve|request_changes")
    expect(p).not.toContain("approve с мелкими замечаниями|request_changes")
    expect(p).not.toContain("approve с мелкими замечаниями (все находки Low) → In Progress")
    // термина выпиленной диспетчеризации в промпте быть не должно
    expect(p).not.toContain("review_type")
  })
  test("result: ветка/ворктри/PR и переходы результата", () => {
    const p = reviewSpawnPrompt("r2", "origa", "result", ctx, "code-quality-reviewer")
    expect(p).toContain("ревью результата")
    expect(p).toContain(ctx.worktree)
    expect(p).toContain(ctx.branch)
    expect(p).toContain(ctx.pr)
    // канон: approve (чистый или дельта) ведёт в «Тестирование», не в Review
    expect(p).toContain("approve (чистый или дельта-апрув) → Тестирование")
    expect(p).toContain("после дельта-апрува → Тестирование")
    expect(p).not.toContain("approve → Review")
    expect(p).not.toContain("→ Review владельцу")
    expect(p).toContain("request_changes → In Progress")
    expect(p).not.toContain("request_changes → Design")
  })
  test("result: approve-with-notes → In Progress с пометкой дельты и прошлых раундов", () => {
    const p = reviewSpawnPrompt("r5", "uwuwu", "result", { branch: "", worktree: "", pr: "" }, "code-quality-reviewer")
    expect(p).toContain("approve с мелкими замечаниями (все находки Low) → In Progress")
    expect(p).toContain("следующее ревью с тем же ревьювером")
    expect(p).toContain("прошлые раунды ревью этой задачи")
  })
  test("result без ворктри (env:false / карантинный ревью-вход) — честная строка про diff/PR, не пустота", () => {
    const p = reviewSpawnPrompt("r3", "uwuwu", "result", { branch: "feat/y", worktree: "", pr: "" }, "code-quality-reviewer")
    expect(p).toContain("Ворктри задачи не найден")
    expect(p).toContain("diff origin/master…feat/y")
    expect(p).not.toContain("Ворктри задачи: ")
  })
  test("result: docs-only hint — диффа/PR может не быть, предмет доки по ссылкам тела", () => {
    // канон docs-only финала (задача 20261109234534): ревьювер не импровизирует
    // по отсутствующему PR, а ревьюит вики-артефакты из тела
    const p = reviewSpawnPrompt("r10", "uwuwu", "result", { branch: "", worktree: "", pr: "" }, "code-quality-reviewer")
    expect(p).toContain("docs-only задача — диффа/PR может не быть")
    expect(p).toContain("доки uwuwu по ссылкам тела")
    // план-ревью без кода — hint не нужен
    const plan = reviewSpawnPrompt("r10", "uwuwu", "plan", { branch: "", worktree: "", pr: "" }, "plan-reviewer")
    expect(plan).not.toContain("docs-only задача")
  })
  test("base из контекста: дифф-строки говорят базу проекта, не дефолт", () => {
    const p = reviewSpawnPrompt("r8", "1xgames", "result", {
      branch: "feat/y", worktree: "", pr: "", base: "origin/main",
    }, "code-quality-reviewer")
    expect(p).toContain("origin/main + untracked")
    expect(p).toContain("diff origin/main…feat/y")
    expect(p).not.toContain("origin/master")
  })
  test("plan без ворктри — строка про diff не появляется (плану код не нужен)", () => {
    const p = reviewSpawnPrompt("r4", "uwuwu", "plan", { branch: "", worktree: "", pr: "" }, "plan-reviewer")
    expect(p).not.toContain("Ворктри задачи не найден")
  })
  test("тип задачи инжектится в ревью-промпт (роутинг Bug/Spike по свойству)", () => {
    const p = reviewSpawnPrompt("r6", "uwuwu", "result", { branch: "", worktree: "", pr: "", type: "bug" }, "code-quality-reviewer")
    expect(p).toContain("Тип задачи: bug")
    const empty = reviewSpawnPrompt("r7", "uwuwu", "result", { branch: "", worktree: "", pr: "", type: "" }, "code-quality-reviewer")
    expect(empty).not.toContain("Тип задачи")
  })
})

describe("reviewRerunPrompt / reviewCommentsPrompt / reviewReturnPrompt", () => {
  test("rerun: статус раунда + переходы + контекст ветки", () => {
    const p = reviewRerunPrompt("r1", "result", { branch: "b", worktree: "/w", pr: "https://pr/1" })
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("устранение своих прошлых замечаний")
    expect(p).toContain("чистый или дельта-апрув → Тестирование")
    expect(p).not.toContain("approve → Review")
    expect(p).toContain("request_changes → In Progress")
    expect(p).toContain("https://pr/1")
  })
  test("rerun: тип задачи инжектится, пустой — не появляется", () => {
    const p = reviewRerunPrompt("r3", "result", { branch: "b", worktree: "", pr: "https://pr/1", type: "spike" })
    expect(p).toContain("Тип задачи: spike")
    const empty = reviewRerunPrompt("r4", "result", { branch: "b", worktree: "", pr: "https://pr/1" })
    expect(empty).not.toContain("Тип задачи")
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
  test("return: approve-with-notes → дофикси и «Ревью результата» с дельтой; чистый approve — тоже «Тестирование»", () => {
    const p = reviewReturnPrompt("r4")
    expect(p).toContain("approve с мелкими замечаниями")
    expect(p).toContain(REVIEW_RESULT_STATUS)
    expect(p).toContain("дельта")
    expect(p).toContain("чистый")
    // канон: оба пути апрува ведут в «Тестирование», мяч юзеру не через Review напрямую
    expect(p).toContain(TESTING_STATUS)
    expect(p).not.toContain("мяч у юзера, от тебя ничего")
  })
  test("return: баг-репорт тестировщика — фикс → «Ревью результата» перед возвратом тестировщику", () => {
    const p = reviewReturnPrompt("r5")
    expect(p).toContain("баг-репорт тестировщика")
    expect(p).toContain(TESTING_STATUS)
    expect(p).toContain("код-ревью обязателен перед возвратом тестировщику")
  })
  test("return: docs-only ветка — «запушь» вакуумных инструкций снят (ревью PR #213, Low)", () => {
    const p = reviewReturnPrompt("r6")
    expect(p).toContain("docs-only задача (правки только в uwuwu-доках)")
    expect(p).toContain("не применяется")
    expect(p).toContain("снова ревью-статус")
  })
})

describe("isValidTracked: ревью- и тест-поля", () => {
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
  test("testSessionID: отсутствует / пусто / ses_ — валидны", () => {
    expect(isValidTracked({ ...base, testSessionID: "" })).toBe(true)
    expect(isValidTracked({ ...base, testSessionID: "ses_t1" })).toBe(true)
  })
  test("testSessionID мусор и testAttempts не-число — невалидны", () => {
    expect(isValidTracked({ ...base, testSessionID: "junk" })).toBe(false)
    expect(isValidTracked({ ...base, testAttempts: "1" })).toBe(false)
    expect(isValidTracked({ ...base, testAttempts: 2 })).toBe(true)
  })
})

describe("sessionSlotOf: адресат агентской сессии по статусу", () => {
  test("ревью-статусы → review, «Тестирование» → test, остальное → dev", () => {
    expect(sessionSlotOf(REVIEW_PLAN_STATUS)).toBe("review")
    expect(sessionSlotOf(REVIEW_RESULT_STATUS)).toBe("review")
    expect(sessionSlotOf(TESTING_STATUS)).toBe("test")
    expect(sessionSlotOf("Design")).toBe("dev")
    expect(sessionSlotOf("In Progress")).toBe("dev")
    expect(sessionSlotOf("Review")).toBe("dev")
    expect(sessionSlotOf("")).toBe("dev")
  })
})

describe("routeTask: стадия «Тестирование»", () => {
  const facts = (over: Partial<RouteFacts>): RouteFacts => ({
    status: TESTING_STATUS,
    lastStatus: TESTING_STATUS,
    reviewerAlive: null,
    testerAlive: null,
    freshComments: [],
    ...over,
  })
  test("вход в «Тестирование» без тест-сессии → спавн тестировщика", () => {
    // первый вход (после approve ревьювера) и повторный вход после баг-петли:
    // мёртвая сессия — всегда спавн (reuse только живой сессии)
    expect(routeTask(facts({ status: TESTING_STATUS, lastStatus: REVIEW_RESULT_STATUS, testerAlive: false })))
      .toEqual({ t: "spawn-tester" })
    expect(routeTask(facts({ status: TESTING_STATUS, lastStatus: TESTING_STATUS, testerAlive: false })))
      .toEqual({ t: "spawn-tester" })
  })
  test("живая тест-сессия + вход/смена статуса → дельта-промпт (rerun)", () => {
    expect(routeTask(facts({ status: TESTING_STATUS, lastStatus: REVIEW_RESULT_STATUS, testerAlive: true })))
      .toEqual({ t: "rerun-tester" })
    expect(routeTask(facts({ status: TESTING_STATUS, lastStatus: "In Progress", testerAlive: true })))
      .toEqual({ t: "rerun-tester" })
  })
  test("живая сессия в том же статусе: комменты юзера → тестировщику, тишина → noop", () => {
    expect(routeTask(facts({ testerAlive: true, freshComments: [c("x", "а скрин где?", 1)] })))
      .toEqual({ t: "comments-to-tester" })
    expect(routeTask(facts({ testerAlive: true }))).toEqual({ t: "noop" })
  })
  test("liveness тест-сессии неизвестна → сессию считаем живой (спавнов не плодим)", () => {
    expect(routeTask(facts({ status: TESTING_STATUS, lastStatus: REVIEW_RESULT_STATUS, testerAlive: null })))
      .toEqual({ t: "rerun-tester" })
  })
  test("fail-вердикт тестировщика: In Progress из «Тестирования» → возврат девелоперу", () => {
    expect(routeTask(facts({ status: "In Progress", lastStatus: TESTING_STATUS })))
      .toEqual({ t: "return-to-dev" })
    expect(routeTask(facts({ status: "Design", lastStatus: TESTING_STATUS })))
      .toEqual({ t: "return-to-dev" })
  })
  test("pass-вердикт тестировщика: Review из «Тестирования» → noop (мяч у юзера)", () => {
    expect(routeTask(facts({ status: "Review", lastStatus: TESTING_STATUS }))).toEqual({ t: "noop" })
    expect(routeTask(facts({ status: "Needs attention", lastStatus: TESTING_STATUS }))).toEqual({ t: "noop" })
  })
})

describe("testingSpawnPrompt / testingRerunPrompt / testingCommentsPrompt", () => {
  const ctx = { branch: "work/task-x", worktree: "/env/x/repo", pr: "https://github.com/x/pull/2" }
  test("spawn: методичка, канал чтения, вердикты pass/fail и переходы", () => {
    const p = testingSpawnPrompt("r1", "uwuwu", ctx)
    expect(p).toContain("стадия тестирования")
    expect(p).toContain("uwuwu-cli task get r1 --project uwuwu")
    expect(p).toContain(ctx.worktree)
    expect(p).toContain(ctx.branch)
    expect(p).toContain(ctx.pr)
    expect(p).toContain("Статус: pass|fail")
    expect(p).toContain("pass → Review")
    expect(p).toContain("fail (баг-репорт с репро-шагами) → In Progress")
    // запрет прод-кода и чистое состояние — contractual строки методички
    expect(p).toContain("прод-код не трогай")
    expect(p).toContain("пуш до вердикта")
  })
  test("spawn без ворктри — честная строка про diff/PR, не пустота", () => {
    const p = testingSpawnPrompt("r2", "uwuwu", { branch: "feat/y", worktree: "", pr: "" })
    expect(p).toContain("Ворктри задачи не найден")
    expect(p).toContain("diff origin/master…feat/y")
  })
  test("spawn: docs-only hint — объект проверки доки по ссылкам тела", () => {
    const p = testingSpawnPrompt("r5", "uwuwu", { branch: "", worktree: "", pr: "" })
    expect(p).toContain("docs-only задача — объект проверки: доки uwuwu по ссылкам тела")
    expect(p).toContain("дифф/PR/CI n/a")
  })
  test("base из контекста: дифф-строки говорят базу проекта, не дефолт", () => {
    const p = testingSpawnPrompt("r9", "1xgames", {
      branch: "feat/y", worktree: "", pr: "", base: "origin/main",
    })
    expect(p).toContain("ветка против origin/main в ворктри задачи")
    expect(p).toContain("diff origin/main…feat/y")
    expect(p).not.toContain("origin/master")
  })
  test("rerun: дельта-режим устранения баг-репорта, не пересмотр с нуля", () => {
    const p = testingRerunPrompt("r3", ctx)
    expect(p).toContain(TESTING_STATUS)
    expect(p).toContain("проверь устранение своего баг-репорта")
    expect(p).toContain("всё заново")
    expect(p).toContain("pass → Review")
    expect(p).toContain("https://github.com/x/pull/2")
  })
  test("comments: цитаты юзера + ответ комментом", () => {
    const p = testingCommentsPrompt("r4", [c("a", "тест упал у меня", 1)])
    expect(p).toContain("стадия тестирования")
    expect(p).toContain("тест упал у меня")
    expect(p).toContain("uwuwu-cli task comment")
  })
})

// ——— resolvePromptWorktree: ворктри для промпт-ctx (env:false резолв) ———

describe("resolvePromptWorktree: env:false-задачи с агентским ворктри не лгут в промпте", () => {
  test("tracked-ворктри не пуст — возвращается он, fs-проба не выполняется", () => {
    // несуществующий envRoot — докажет, что до пробы дело не дошло
    expect(resolvePromptWorktree("r1", "/env/r1/repo", "/nonexistent-root")).toBe("/env/r1/repo")
  })
  test("env:false с живым агентским ворктри по канон-пути — путь резолвится", () => {
    const root = mkdtempSync(join(tmpdir(), "wt-probe-"))
    try {
      const repoDir = join(root, "20261108081220-pwvf1fn", "repo")
      mkdirSync(repoDir, { recursive: true })
      writeFileSync(join(repoDir, ".git"), "gitdir: /elsewhere/main/.git/worktrees/x\n")
      expect(resolvePromptWorktree("20261108081220-pwvf1fn", "", root)).toBe(repoDir)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("каталога нет — пусто: фоллбэк-строка промпта честна", () => {
    const root = mkdtempSync(join(tmpdir(), "wt-probe-"))
    try {
      expect(resolvePromptWorktree("20261109005428-g9wx5af", "", root)).toBe("")
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })
  test("обломок без .git — пусто, битый путь агенту не скармливается", () => {
    const root = mkdtempSync(join(tmpdir(), "wt-probe-"))
    try {
      mkdirSync(join(root, "rowz", "repo"), { recursive: true })
      expect(resolvePromptWorktree("rowz", "", root)).toBe("")
      expect(existsSync(join(root, "rowz", "repo"))).toBe(true)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
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

  test("base: строка проходит и сохраняется, отсутствие — дефолт (master-based репо не меняют поведение)", () => {
    const raw = JSON.stringify({ test: { board: "b", repo: "r", local: "/l", base: "origin/main" } })
    expect(parseProjects(raw, fallback).test?.base).toBe("origin/main")
    expect(parseProjects(undefined, fallback).uwuwu?.base).toBeUndefined()
  })

  test("base: не-строка или пустая — отказ с именем проекта", () => {
    const nonString = JSON.stringify({ test: { board: "b", repo: "r", local: "/l", base: 42 } })
    expect(() => parseProjects(nonString, fallback)).toThrow(/test.*base/)
    const empty = JSON.stringify({ test: { board: "b", repo: "r", local: "/l", base: "  " } })
    expect(() => parseProjects(empty, fallback)).toThrow(/test.*base/)
  })
})

describe("baseOf: база из конфига/контекста с дефолтом DEFAULT_BASE", () => {
  test("нет base — дефолт origin/master (DEFAULT_PROJECTS без поля не меняют поведение)", () => {
    expect(baseOf({})).toBe("origin/master")
    expect(baseOf({ base: undefined })).toBe(DEFAULT_BASE)
  })
  test("заданная base — она и возвращается", () => {
    expect(baseOf({ base: "origin/main" })).toBe("origin/main")
  })
})

// ——— сводка прохода ———

describe("formatPassSummary: одна строка о проходе", () => {
  test("полная сводка с фейлами перечисляет причины", () => {
    const line = formatPassSummary("uwuwu", {
      devSpawned: 4, reviewSpawned: 1, testerSpawned: 1, wakeups: 2, cleanups: 1,
      envCreated: 2, envRemoved: 1, envAtticked: 1, watched: 12,
      failures: ["20261105ab: сессия мертва после спавна"],
    })
    expect(line).toBe(
      "проход uwuwu: спавн dev=4 ревью=1 тест=1, будильники=2, чистки=1, env=+2/-1/attic:1, наблюдение=12" +
      ", фейлы=1 (20261105ab: сессия мертва после спавна)")
  })

  test("тихий тик без событий всё равно виден (наблюдение=0)", () => {
    const line = formatPassSummary("origa", {
      devSpawned: 0, reviewSpawned: 0, testerSpawned: 0, wakeups: 0, cleanups: 0,
      envCreated: 0, envRemoved: 0, envAtticked: 0,
      watched: 0, failures: [],
    })
    expect(line).toBe("проход origa: спавн dev=0 ревью=0 тест=0, будильники=0, чистки=0, env=+0/-0/attic:0, наблюдение=0")
  })

  test("задержанные гардом спавны видны в сводке", () => {
    const line = formatPassSummary("uwuwu", {
      devSpawned: 0, reviewSpawned: 0, testerSpawned: 0, wakeups: 0, cleanups: 0,
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
  test("таблица: дефолт 360 (6 ч) на пустоту/мусор, валидное число — оно", () => {
    expect(stallMinutesFromEnv(undefined)).toBe(360)
    expect(stallMinutesFromEnv("")).toBe(360)
    expect(stallMinutesFromEnv("  ")).toBe(360)
    expect(stallMinutesFromEnv("15")).toBe(15)
    expect(stallMinutesFromEnv("7.5")).toBe(7.5)
    expect(stallMinutesFromEnv("0")).toBe(360) // сигнал на каждой паузе — мёртвый режим
    expect(stallMinutesFromEnv("-5")).toBe(360)
    expect(stallMinutesFromEnv("abc")).toBe(360)
    expect(stallMinutesFromEnv("NaN")).toBe(360)
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
    lastDevPromptAt: null,
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
  test("ровно на пороге — ещё none (возраст строго больше порога)", () => {
    expect(stallVerdict(facts({ lastActivityAt: NOW - 30 * MIN }), 30 * MIN)).toEqual({ t: "none" })
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
})

describe("stallClock: часы тишины сталла — от последнего сигнала сессии", () => {
  const NOW = 1_000_000_000_000
  const MIN = 60_000

  test("часы от активности: в пределах порога — не застой, за порогом — застой (прежнее поведение)", () => {
    const fresh = stallClock({ lastActivityAt: NOW - 5 * MIN, lastDevPromptAt: null }, 30 * MIN, NOW)
    expect(fresh.stalled).toBe(false)
    expect(fresh.quietMin).toBe(5)
    expect(fresh.fromWake).toBe(false)
    const stale = stallClock({ lastActivityAt: NOW - 40 * MIN, lastDevPromptAt: null }, 30 * MIN, NOW)
    expect(stale.stalled).toBe(true)
    expect(stale.quietMin).toBe(40)
  })
  test("свежий будильник сбрасывает часы: активность старая, промпт свежий — не застой (инцидент 09.11)", () => {
    const c = stallClock({ lastActivityAt: NOW - 507 * MIN, lastDevPromptAt: NOW - MIN }, 360 * MIN, NOW)
    expect(c.stalled).toBe(false)
    expect(c.quietMin).toBe(1)
    expect(c.fromWake).toBe(true)
  })
  test("будка умерла: тишина от промпта больше порога — застой", () => {
    const c = stallClock({ lastActivityAt: NOW - 500 * MIN, lastDevPromptAt: NOW - 400 * MIN }, 360 * MIN, NOW)
    expect(c.stalled).toBe(true)
    expect(c.quietMin).toBe(400)
    expect(c.fromWake).toBe(true)
  })
  test("ответил после будки: часы снова от активности", () => {
    const c = stallClock({ lastActivityAt: NOW - 2 * MIN, lastDevPromptAt: NOW - 10 * MIN }, 30 * MIN, NOW)
    expect(c.stalled).toBe(false)
    expect(c.quietMin).toBe(2)
    expect(c.fromWake).toBe(false)
  })
  test("оба null — застой с рождения, quietMin null, fromWake false", () => {
    const c = stallClock({ lastActivityAt: null, lastDevPromptAt: null }, 30 * MIN, NOW)
    expect(c.stalled).toBe(true)
    expect(c.quietMin).toBeNull()
    expect(c.fromWake).toBe(false)
  })
})

describe("stallVerdict: часы тишины учитывают последний промпт в дев-сессию", () => {
  const NOW = 1_000_000_000_000
  const MIN = 60_000
  const facts = (over: Partial<StallFacts> = {}): StallFacts => ({
    status: "In Progress",
    sessionAlive: true,
    lastActivityAt: NOW - 507 * MIN,
    lastDevPromptAt: null,
    branch: "work/task-abc",
    branchInWorktree: true,
    prMerged: false,
    signaled: false,
    now: NOW,
    ...over,
  })

  test("свежий будильник при старой активности — none: wake+kick одним тиком исключён (инцидент 09.11)", () => {
    expect(stallVerdict(facts({ lastDevPromptAt: NOW - MIN }), 360 * MIN)).toEqual({ t: "none" })
  })
  test("будка умерла: тишина от wake больше порога — signal, тихий текст от точки часов", () => {
    const v = stallVerdict(facts({ lastDevPromptAt: NOW - 400 * MIN }), 360 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("тишина 400 мин")
    expect(v.text).not.toContain("507")
  })
})

describe("bodyEditRoute: адресат будки контентной правки тела", () => {
  const DEV = "ses_dev"
  const REVIEWER = "ses_rev"
  const TESTER = "ses_tst"
  const facts = (over: Partial<Parameters<typeof bodyEditRoute>[0]> = {}) => ({
    status: "In Progress",
    reviewSessionID: REVIEWER,
    testSessionID: TESTER,
    devSessionID: DEV,
    ...over,
  })

  test("Review — swallow с обновлением базлайна: мяч у владельца, будить дева нечего (инцидент 09.11)", () => {
    expect(bodyEditRoute(facts({ status: USER_REVIEW_STATUS })))
      .toEqual({ t: "swallow", updateBaseline: true })
  })
  test("ревью-статус при живом ревьювере — ревьюверу", () => {
    for (const status of [REVIEW_PLAN_STATUS, REVIEW_RESULT_STATUS]) {
      expect(bodyEditRoute(facts({ status }))).toEqual({ t: "wake", target: REVIEWER, role: "ревьювер" })
    }
  })
  test("Тестирование при живом тестировщике — тестировщику", () => {
    expect(bodyEditRoute(facts({ status: TESTING_STATUS })))
      .toEqual({ t: "wake", target: TESTER, role: "тестировщик" })
  })
  test("In Progress и Design — деву", () => {
    for (const status of ["In Progress", "Design"]) {
      expect(bodyEditRoute(facts({ status }))).toEqual({ t: "wake", target: DEV, role: "девелопер" })
    }
  })
  test("держатель умер в агентской стадии — фоллбек на дева (прежнее поведение проводки)", () => {
    expect(bodyEditRoute(facts({ status: REVIEW_RESULT_STATUS, reviewSessionID: "" })))
      .toEqual({ t: "wake", target: DEV, role: "девелопер" })
    expect(bodyEditRoute(facts({ status: TESTING_STATUS, testSessionID: "" })))
      .toEqual({ t: "wake", target: DEV, role: "девелопер" })
  })
  test("дев в карантине вне агентской стадии — swallow без базлайна (ждём адресата)", () => {
    expect(bodyEditRoute(facts({ devSessionID: "" })))
      .toEqual({ t: "swallow", updateBaseline: false })
  })
})

describe("prProbeVerdict: merged-проба с пределом деградации unknown", () => {
  test("merged/open — вердикт без дефера и деградации, streak сброшен", () => {
    expect(prProbeVerdict("merged", 2)).toEqual({ merged: true, defer: false, streakAfter: 0, degraded: false })
    expect(prProbeVerdict("open", 2)).toEqual({ merged: false, defer: false, streakAfter: 0, degraded: false })
  })
  test("unknown до предела — кик откладывается на тик (defer: сигнал не строится)", () => {
    expect(prProbeVerdict("unknown", 0)).toEqual({ merged: false, defer: true, streakAfter: 1, degraded: false })
    expect(prProbeVerdict("unknown", PR_PROBE_UNKNOWN_MAX - 2))
      .toEqual({ merged: false, defer: true, streakAfter: PR_PROBE_UNKNOWN_MAX - 1, degraded: false })
  })
  test("unknown на пределе и дальше — деградация к not-merged, кик идёт (страховка ADR-044 возвращена)", () => {
    expect(prProbeVerdict("unknown", PR_PROBE_UNKNOWN_MAX - 1))
      .toEqual({ merged: false, defer: false, streakAfter: PR_PROBE_UNKNOWN_MAX, degraded: true })
    expect(prProbeVerdict("unknown", PR_PROBE_UNKNOWN_MAX + 4))
      .toEqual({ merged: false, defer: false, streakAfter: PR_PROBE_UNKNOWN_MAX + 5, degraded: true })
  })
  test("удачная проба после полосы unknown — streak сброшен", () => {
    expect(prProbeVerdict("open", PR_PROBE_UNKNOWN_MAX))
      .toEqual({ merged: false, defer: false, streakAfter: 0, degraded: false })
  })
})

describe("isValidTracked: поля сталл-часов и пробы PR", () => {
  const base = {
    sessionID: "ses_abc", worktree: "/wt/task-x", branch: "work/task-x",
    bodyDoc: "d1", title: "t", project: "uwuwu", lastCommentAt: 1, lastCommentId: "i", createdAt: 2, attempts: 0,
  }
  test("lastDevPromptAt: число и отсутствие — валидны, не-число — мимо", () => {
    expect(isValidTracked({ ...base, lastDevPromptAt: 1234 })).toBe(true)
    expect(isValidTracked({ ...base, lastDevPromptAt: undefined })).toBe(true)
    expect(isValidTracked({ ...base, lastDevPromptAt: "1234" })).toBe(false)
  })
  test("prProbeUnknownStreak: число и отсутствие — валидны, не-число — мимо", () => {
    expect(isValidTracked({ ...base, prProbeUnknownStreak: 2 })).toBe(true)
    expect(isValidTracked({ ...base, prProbeUnknownStreak: undefined })).toBe(true)
    expect(isValidTracked({ ...base, prProbeUnknownStreak: true })).toBe(false)
  })
})

describe("reviewStallVerdict: недоставленный вердикт держателя стадии (инцидент 07.11)", () => {
  const NOW = 1_000_000_000_000
  const MIN = 60_000
  const facts = (over: Partial<ReviewStallFacts> = {}): ReviewStallFacts => ({
    status: REVIEW_RESULT_STATUS,
    holderAlive: true,
    lastActivityAt: NOW - 40 * MIN,
    signaled: false,
    now: NOW,
    ...over,
  })

  test("агентская стадия + живой держатель + тишина > порога — signal; текст ведёт к будилке", () => {
    const v = reviewStallVerdict(facts(), 30 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("[factory]")
    expect(v.text).toContain("тишина 40 мин")
    expect(v.text).toContain("пороге 30 мин")
    // сценарий вопроса юзеру: ответ комментом — но перестановка статуса
    // обязательный шаг обоих сценариев (коммент в Needs attention агенту
    // не доставляется — доставка только в агентском статусе)
    expect(v.text).toContain("ответь комментом")
    expect(v.text).toContain("Затем переставь статус стадии")
    expect(v.text).toContain("треды")
  })
  test("оба ревью-статуса покрываются (план и результат)", () => {
    expect(reviewStallVerdict(facts({ status: REVIEW_PLAN_STATUS }), 30 * MIN).t).toBe("signal")
    expect(reviewStallVerdict(facts({ status: REVIEW_RESULT_STATUS }), 30 * MIN).t).toBe("signal")
  })
  test("«Тестирование» покрывается — текст называет тестировщика, не ревьювера", () => {
    const v = reviewStallVerdict(facts({ status: TESTING_STATUS }), 30 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("сессия тестировщика молчит")
    expect(v.text).not.toContain("ревьювера")
  })
  test("не агентская стадия — none даже при застое", () => {
    expect(reviewStallVerdict(facts({ status: "In Progress" }), 30 * MIN)).toEqual({ t: "none" })
    expect(reviewStallVerdict(facts({ status: "Needs attention" }), 30 * MIN)).toEqual({ t: "none" })
    expect(reviewStallVerdict(facts({ status: "Design" }), 30 * MIN)).toEqual({ t: "none" })
    expect(reviewStallVerdict(facts({ status: "Review" }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("свежая активность держателя — none (ход ещё идёт или вернулся)", () => {
    expect(reviewStallVerdict(facts({ lastActivityAt: NOW - 5 * MIN }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("dead держатель — none (routeTask сам спавнит), unknown — none (пропуск тика)", () => {
    expect(reviewStallVerdict(facts({ holderAlive: false }), 30 * MIN)).toEqual({ t: "none" })
    expect(reviewStallVerdict(facts({ holderAlive: null }), 30 * MIN)).toEqual({ t: "none" })
  })
  test("повторный тик после сигнала — silent (анти-спам раз за эпизод)", () => {
    expect(reviewStallVerdict(facts({ signaled: true }), 30 * MIN)).toEqual({ t: "silent" })
  })
  test("активность вернулась при стоявшем флаге — none (демон снимет флаг)", () => {
    expect(reviewStallVerdict(facts({ signaled: true, lastActivityAt: NOW - 2 * MIN }), 30 * MIN))
      .toEqual({ t: "none" })
  })
  test("сообщений нет вовсе — signal (спавн не начал ход или потерялся)", () => {
    const v = reviewStallVerdict(facts({ lastActivityAt: null }), 30 * MIN)
    expect(v.t).toBe("signal")
    if (v.t !== "signal") return
    expect(v.text).toContain("сообщений в сессии нет")
  })
  test("ровно на пороге — ещё none (возраст строго больше порога)", () => {
    expect(reviewStallVerdict(facts({ lastActivityAt: NOW - 30 * MIN }), 30 * MIN)).toEqual({ t: "none" })
  })
})

// Дефолт-порог 6 ч (трейд владельца 08.11, задача 20261108161255-rmw1ryj):
// глубокие фоновые прогоны — сборки/e2e/глубокое тестирование — легитимно
// молчат часами. Реплика инцидента 07.11 (задача 20261107154637): агент ушёл
// в холодную Railway-сборку, фабрика дала ложный stall на 31-й минуте при
// ~90 минутах реальной работы. Оба вердикта на ПРОИЗВОДСТВЕННОМ дефолте.
describe("дефолт-порог 6 ч: реплика инцидента 07.11 и настоящий завис", () => {
  const NOW = 1_000_000_000_000
  const MIN = 60_000
  const DEFAULT_MIN = stallMinutesFromEnv(undefined)
  const DEFAULT_MS = DEFAULT_MIN * MIN // вердикты принимают миллисекунды (STALL_MS = STALL_MIN * 60_000)
  const devFacts = (lastActivityAt: number | null): StallFacts => ({
    status: "In Progress",
    sessionAlive: true,
    lastActivityAt,
    lastDevPromptAt: null,
    branch: "work/task-abc",
    branchInWorktree: true,
    prMerged: false,
    signaled: false,
    now: NOW,
  })
  const holderFacts = (lastActivityAt: number | null): ReviewStallFacts => ({
    status: TESTING_STATUS,
    holderAlive: true,
    lastActivityAt,
    signaled: false,
    now: NOW,
  })

  test("порог по умолчанию — 360 мин (6 ч)", () => {
    expect(DEFAULT_MIN).toBe(360)
  })
  test("реплика 07.11: тишина 31 мин (ложный сталл инцидента) — none в обоих вердиктах", () => {
    expect(stallVerdict(devFacts(NOW - 31 * MIN), DEFAULT_MS).t).toBe("none")
    expect(reviewStallVerdict(holderFacts(NOW - 31 * MIN), DEFAULT_MS).t).toBe("none")
  })
  test("фоновая сборка ~90 мин (дамп сессии 07.11) — none", () => {
    expect(stallVerdict(devFacts(NOW - 90 * MIN), DEFAULT_MS).t).toBe("none")
    expect(reviewStallVerdict(holderFacts(NOW - 90 * MIN), DEFAULT_MS).t).toBe("none")
  })
  test("глубокое тестирование 5 ч — none (класс жалобы владельца)", () => {
    expect(reviewStallVerdict(holderFacts(NOW - 5 * 60 * MIN), DEFAULT_MS).t).toBe("none")
  })
  test("настоящее зависание: тишина 6 ч + 1 мин — signal в обоих вердиктах", () => {
    expect(stallVerdict(devFacts(NOW - 361 * MIN), DEFAULT_MS).t).toBe("signal")
    expect(reviewStallVerdict(holderFacts(NOW - 361 * MIN), DEFAULT_MS).t).toBe("signal")
  })
  test("env-ручка: UWUWU_FACTORY_STALL_MIN=30 возвращает старый порог — тишина 31 мин снова ловится", () => {
    // откат порога на машине без кода (ручка совместимости, PR #182):
    // парсер → вердикт целиком, не только таблица парсера
    const old30Ms = stallMinutesFromEnv("30") * MIN
    expect(stallVerdict(devFacts(NOW - 31 * MIN), old30Ms).t).toBe("signal")
    expect(reviewStallVerdict(holderFacts(NOW - 31 * MIN), old30Ms).t).toBe("signal")
  })
  test("env-ручка: порог 90 мин закрывает окно фоновых сборок — 31 и 90 мин молчат, 91 ловится", () => {
    const p90Ms = stallMinutesFromEnv("90") * MIN
    expect(reviewStallVerdict(holderFacts(NOW - 90 * MIN), p90Ms).t).toBe("none")
    expect(reviewStallVerdict(holderFacts(NOW - 91 * MIN), p90Ms).t).toBe("signal")
  })
  test("ровно 6 ч — ещё none (возраст строго больше порога)", () => {
    expect(stallVerdict(devFacts(NOW - 360 * MIN), DEFAULT_MS).t).toBe("none")
    expect(reviewStallVerdict(holderFacts(NOW - 360 * MIN), DEFAULT_MS).t).toBe("none")
  })
})

describe("adoptBodyDoc: самоисцеление пустого bodyDoc трека", () => {
  const tracked = (bodyDoc: string): TrackedTask => ({
    sessionID: "ses_x", worktree: "", branch: "", bodyDoc,
    title: "Задача", lastCommentAt: 0, lastCommentId: "",
    createdAt: 1, attempts: 0, project: "t",
  })
  const row = (bodyDoc: string): TaskRef => ({
    rowId: "row1", bodyDoc, title: "Задача", status: "In Progress", type: "", pr: "", branch: "",
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
  bodyDoc: "", title: "Задача", status: "Design", type: "", pr: "", branch: "",
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
  const cell = (devSession?: string, reviewSession?: string, testSession?: string)
    : Pick<TaskRef, "devSession" | "reviewSession" | "testSession"> => ({
    devSession,
    reviewSession,
    testSession,
  })

  test("чистая fresh-строка без свидетельств — спавн обычного пути, не сирота", () => {
    expect(untrackedRowDecision(cell(undefined, undefined), "alive")).toEqual({ devWave: true, orphan: false })
    expect(untrackedRowDecision(cell("", "", ""), "dead")).toEqual({ devWave: true, orphan: false })
  })

  test("ревью-свидетельство без трека (дев пуста) — сирота как прежде, защита инцидента 06.11", () => {
    expect(untrackedRowDecision(cell(undefined, "ses_rev01"), "dead")).toEqual({ devWave: true, orphan: true })
    expect(untrackedRowDecision(cell("", "ses_rev01"), "alive")).toEqual({ devWave: true, orphan: true })
  })

  test("тест-свидетельство без трека — сирота (паритет ревью-свидетельства)", () => {
    expect(untrackedRowDecision(cell(undefined, undefined, "ses_test01"), "dead"))
      .toEqual({ devWave: true, orphan: true })
    expect(untrackedRowDecision(cell("", "", "ses_test01"), "alive"))
      .toEqual({ devWave: true, orphan: true })
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

// ——— self-эхо wakeup-контура (задача 20261106032707): маркеры, время, сесс-скопед срез ———

describe("stripAgentHead — снятие CLI-обёртки перед маркерами", () => {
  test("голова агента снимается — маркер [factory] становится видимым", () => {
    expect(stripAgentHead("AGENTS (2026-11-05 19:43): [factory] не смог завестись"))
      .toBe("[factory] не смог завестись")
  })
  test("тред-префикс и голова снимаются вместе", () => {
    expect(stripAgentHead("(тред-ответ на «вопрос») AGENTS (2026-11-06 15:59): 🤖 отчёт"))
      .toBe("🤖 отчёт")
  })
  test("без агентской головы текст не меняется", () => {
    expect(stripAgentHead("юзерский текст без обёртки")).toBe("юзерский текст без обёртки")
    expect(stripAgentHead("Юра (2026-11-05 20:00): [factory] моя цитата маркера"))
      .toBe("Юра (2026-11-05 20:00): [factory] моя цитата маркера")
  })
  test("обёрнутые фабричный и бот-комменты — не юзерские (дефект №2)", () => {
    expect(isUserComment(c("1", "AGENTS (2026-11-05 19:43): [factory] привет", 1))).toBe(false)
    expect(isUserComment(c("2", "(тред-ответ на «x») AGENTS (2026-11-06 16:13): 🤖 доклад", 1))).toBe(false)
  })
  test("юзерские блок-цитата и обёртка — юзерские", () => {
    expect(isUserComment(c("3", "> [factory] цитата владельца", 1))).toBe(true)
    expect(isUserComment(c("4", "Юра (2026-11-05 20:00): делай так", 1))).toBe(true)
  })
})

describe("parseAgentHeadTs — время коммента из CLI-обёртки (UTC)", () => {
  test("UTC-парс: локальная зона рантайма (демон в JST) не влияет", () => {
    expect(parseAgentHeadTs("AGENTS (2026-11-05 19:43): привет"))
      .toBe(Date.UTC(2026, 10, 5, 19, 43))
  })
  test("ts с тред-префиксом и с секундами парсится", () => {
    expect(parseAgentHeadTs("(тред-ответ на «x») AGENTS (2026-11-06 16:13): hi"))
      .toBe(Date.UTC(2026, 10, 6, 16, 13))
    expect(parseAgentHeadTs("AGENTS (2026-11-06 16:13:45): hi"))
      .toBe(Date.UTC(2026, 10, 6, 16, 13, 45))
  })
  test("без головы или с битым ts — null", () => {
    expect(parseAgentHeadTs("юзерский текст без обёртки")).toBeNull()
    expect(parseAgentHeadTs("AGENTS (завтра утром): hi")).toBeNull()
    expect(parseAgentHeadTs("")).toBeNull()
  })
  test("компоненты вне диапазонов (перелив Date.UTC) — null", () => {
    expect(parseAgentHeadTs("AGENTS (2026-13-45 99:99): hi")).toBeNull()
    expect(parseAgentHeadTs("AGENTS (2026-11-05 24:00): hi")).toBeNull()
    expect(parseAgentHeadTs("AGENTS (2026-02-30 10:00): hi")).toBeNull()
    expect(parseAgentHeadTs("AGENTS (2026-11-05 10:60): hi")).toBeNull()
  })
})

describe("freshForeignComments — сесс-скопед срез self-эха (дефект №1)", () => {
  const own = (id: string, at: number): TaskComment => ({ id, text: "AGENTS (t): мой доклад", createdAt: at, userId: "AGENTS" })
  const user = (id: string, at: number): TaskComment => ({ id, text: "ответ владельца", createdAt: at, userId: "Юра" })
  test("собственный коммент сессии (ts ≤ активности) срезается", () => {
    const fresh = [own("1", 1000), user("2", 2000)]
    const { foreign, ownCount } = freshForeignComments(fresh, 2500)
    expect(ownCount).toBe(1)
    expect(foreign.map((x) => x.id)).toEqual(["2"])
  })
  test("чужой AGENTS-коммент (ts > активности) будит", () => {
    const fresh = [own("1", 5000), user("2", 6000)]
    const { foreign, ownCount } = freshForeignComments(fresh, 2500)
    expect(ownCount).toBe(0)
    expect(foreign.map((x) => x.id)).toEqual(["1", "2"])
  })
  test("юзерский коммент не срезается даже внутри интервала активности", () => {
    const fresh = [user("2", 1500)]
    const { foreign, ownCount } = freshForeignComments(fresh, 2500)
    expect(ownCount).toBe(0)
    expect(foreign.length).toBe(1)
  })
  test("активность неизвестна — без среза (эхо-будка вместо потери коммента)", () => {
    const fresh = [own("1", 1000)]
    const { foreign, ownCount } = freshForeignComments(fresh, null)
    expect(ownCount).toBe(0)
    expect(foreign.length).toBe(1)
  })
  test("легаси ts=0 не срезается — неизвестное время не доказательство «своего»", () => {
    const fresh = [own("1", 0)]
    const { foreign, ownCount } = freshForeignComments(fresh, 999_999)
    expect(ownCount).toBe(0)
    expect(foreign.length).toBe(1)
  })
  test("граница ts == активности срезается (коммит в момент завершения хода)", () => {
    const { ownCount } = freshForeignComments([own("1", 2500)], 2500)
    expect(ownCount).toBe(1)
  })
})

describe("legacyRebaselineNeeded — переход легаси-треков после деплоя", () => {
  const users = [c("cDOC-0000", "старый юзерский", Date.UTC(2026, 10, 5, 10, 0))]
  test("старый формат id при нулевом времени — нужен re-baseline", () => {
    expect(legacyRebaselineNeeded({ at: 0, id: "cDOC-3" }, users)).toBe(true)
  })
  test("свежий спавн (пустая граница) — не нужен", () => {
    expect(legacyRebaselineNeeded({ at: 0, id: "" }, users)).toBe(false)
  })
  test("несматченный id при ненулевом времени — обычный фоллбек по времени", () => {
    expect(legacyRebaselineNeeded({ at: 500, id: "cDOC-3" }, users)).toBe(false)
  })
  test("сматченный id при нулевом времени — не нужен", () => {
    expect(legacyRebaselineNeeded({ at: 0, id: "cDOC-0000" }, users)).toBe(false)
  })
  test("пустой юзерский срез — нужен: сброс мёртвой границы, чтобы будущие юзерские будили", () => {
    // при false легаси-id остался бы навсегда: первый новый юзерский коммент
    // съелся бы re-baseline'ом без будки
    expect(legacyRebaselineNeeded({ at: 0, id: "cDOC-3" }, [])).toBe(true)
  })
})

describe("latestComment — максимум по (createdAt, id)", () => {
  test("последний по времени, тай-брейк по id", () => {
    expect(latestComment([c("b", "t", 100), c("c", "t", 300), c("a", "t", 200)])?.id).toBe("c")
    expect(latestComment([c("cD-0009", "t", 100), c("cD-0010", "t", 100)])?.id).toBe("cD-0010")
    expect(latestComment([])).toBeNull()
  })
})

describe("normalizeBodyMd + bodyContentHash — контент-хэш вне «Комменты» (№3/№5)", () => {
  const frontmatter = (lastmod: string): string =>
    `---\ntitle: t\ndate: d\nlastmod: ${lastmod}\n---\n\n# Заголовок\n\nТекст **bold**.\n`
  test("lastmod-only пересохранение — хэш не меняется (открытая вкладка владельца)", () => {
    const a = bodyContentHash(frontmatter("2026-11-06T15:27:00+09:00"))
    const b = bodyContentHash(frontmatter("2026-11-06T16:00:00+09:00"))
    expect(a).toBe(b)
  })
  test("артефакты round-trip (ZWJ, \\r\\n, trailing, пустые строки) — хэш не меняется", () => {
    const a = "# H\n\nТекст **bold**.\n"
    const b = "# H\r\n\r\n\r\nТекст \u200b**bold**\u200d.   \r\n\r\n\r\n"
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("контентная правка вне «Комменты» — хэш меняется", () => {
    const a = "# H\n\nТекст A.\n"
    const b = "# H\n\nТекст B.\n"
    expect(bodyContentHash(b)).not.toBe(bodyContentHash(a))
  })
  test("пробелы после bold-маркера (UI-ренормализация) — хэш не меняется", () => {
    const a = "**Обратная совместимость:** state валиден\n"
    const b = "**Обратная совместимость:**  state валиден\n"
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("перестройка таблиц (пэддинг ячеек, separator) — хэш не меняется", () => {
    const a = "|Проверка|Статус|\n|---|---|\n|Tests|✅|\n"
    const b = "| Проверка    | Статус   |\n| ------------------| --------------|\n| Tests       | ✅       |\n"
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("содержательная правка ячейки таблицы — хэш меняется", () => {
    const a = "|Tests|✅|\n"
    const b = "|Tests|❌|\n"
    expect(bodyContentHash(b)).not.toBe(bodyContentHash(a))
  })
  test("аппенд коммента в «Комменты» — хэш не меняется (№4)", () => {
    const a = "# План\n\nтело\n\n## Комменты\n\n- **AGENTS** (t1): вопрос\n"
    const b = "# План\n\nтело\n\n## Комменты\n\n- **AGENTS** (t1): вопрос\n- **AGENTS** (t2): вердикт\n"
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("трейлинг-хвост комментов в конце дока (CLI-аппенд мимо секции) — хэш не меняется", () => {
    const a = "## План\n\nтело\n\n## Комменты\n\n- **AGENTS** (t0): старый\n\n## Дефект\n\nтекст дефекта\n"
    const b = `${a}\n- **AGENTS** (2026-11-06 16:46): вердикт ревьювера\n\n`
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("контентная строка после хвостового коммента неразличима (коммент-область до EOF)", () => {
    const a = "## План\n\nтело\n\n- **AGENTS** (2026-11-06 16:46): коммент\n"
    const b = `${a}добавленная контентная строка\n`
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("легитимный список без даты в скобках хвостом не срезается", () => {
    const a = "## План\n\n- **жирный** пункт списка\n"
    const b = `${a}- **жирный** ещё пункт\n`
    expect(bodyContentHash(b)).not.toBe(bodyContentHash(a))
  })
  test("трейлинг-хвост многострочных коммент-блоков срезается целиком (тела вердиктов)", () => {
    const a = "## План\n\nтело\n\n## Дефект\n\nтекст дефекта\n"
    const b = `${a}\n- **AGENTS** (2026-11-06 16:39): PR готов к ревью\n\nподробности\n\n- **AGENTS** (2026-11-06 16:46): вердикт\n\nСтатус: approve\n\nабзацы отчёта\n`
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("коммент-аппенд в хвост с контентным абзацем — хэш не меняется (абзац сохранён)", () => {
    const a = "## Отчёт\n\nабзац отчёта\n"
    const b = `${a}\n- **AGENTS** (2026-11-06 16:46): коммент\n\nСтатус: approve\n`
    expect(bodyContentHash(b)).toBe(bodyContentHash(a))
  })
  test("правка абзаца хвоста до первого коммент-пункта — хэш меняется", () => {
    const a = "## Отчёт\n\nабзац А\n\n- **AGENTS** (2026-11-06 16:46): коммент\n"
    const b = "## Отчёт\n\nабзац Б\n\n- **AGENTS** (2026-11-06 16:46): коммент\n"
    expect(bodyContentHash(b)).not.toBe(bodyContentHash(a))
  })
  test("контент ПОСЛЕ секции «Комменты» учитывается (правка дефектов видна)", () => {
    const a = "## План\n\nтело\n\n## Комменты\n\n- **AGENTS** (t): c\n\n## Дефект\n\nстарый текст\n"
    const b = "## План\n\nтело\n\n## Комменты\n\n- **AGENTS** (t): c\n\n## Дефект\n\nновый текст\n"
    expect(bodyContentHash(b)).not.toBe(bodyContentHash(a))
  })
  test("bodyLastmodMs — парс lastmod с суффиксом зоны; без него — null", () => {
    expect(bodyLastmodMs(frontmatter("2026-11-06T15:27:00+09:00")))
      .toBe(Date.parse("2026-11-06T15:27:00+09:00"))
    expect(bodyLastmodMs("# без шапки\n")).toBeNull()
  })
})

describe("wakeupDedup — дедуп повторной доставки (№5.2)", () => {
  const now = 1_000_000
  const window = 10 * 60_000
  const stamp = (key: string, at: number) => ({ key, at })
  test("первая доставка проходит", () => {
    expect(wakeupDedup(undefined, "body:h1", now, window)).toBe("deliver")
  })
  test("идентичный ключ в окне — скип", () => {
    expect(wakeupDedup(stamp("body:h1", now - 1000), "body:h1", now, window)).toBe("skip")
  })
  test("идентичный ключ вне окна — проходит", () => {
    expect(wakeupDedup(stamp("body:h1", now - window - 1), "body:h1", now, window)).toBe("deliver")
  })
  test("другой ключ в окне — проходит", () => {
    expect(wakeupDedup(stamp("comment:x", now - 1000), "body:h1", now, window)).toBe("deliver")
  })
})

describe("isValidTracked: поля bodyHash/lastWakeup", () => {
  const base = {
    sessionID: "ses_abc", worktree: "/wt/x", branch: "work/x",
    bodyDoc: "d1", title: "t", project: "uwuwu", lastCommentAt: 1, lastCommentId: "i", createdAt: 2, attempts: 0,
  }
  test("строка-хэш и штамп {key, at} валидны; undefined валиден", () => {
    expect(isValidTracked({ ...base, bodyHash: "abc", lastWakeup: { key: "body:h", at: 1 } })).toBe(true)
    expect(isValidTracked({ ...base, bodyHash: undefined, lastWakeup: undefined })).toBe(true)
  })
  test("чужие типы — мимо", () => {
    expect(isValidTracked({ ...base, bodyHash: 5 })).toBe(false)
    expect(isValidTracked({ ...base, lastWakeup: { key: "k" } })).toBe(false)
    expect(isValidTracked({ ...base, lastWakeup: { key: "k", at: "x" } })).toBe(false)
    expect(isValidTracked({ ...base, lastWakeup: "body:h" })).toBe(false)
  })
})

describe("parseCommentsMarkdown — секция «Комменты» (SiYuan-эмуляция list_comments)", () => {
  const MD = [
    "## Постановка", "", "- пункт тела", "",
    "## Комменты", "",
    "- **AGENTS** (2026-11-05 19:43): вопрос агента",
    "  - **Юра** (2026-11-05 20:00): ответ владельца",
    "- **AGENTS** (2026-11-06 10:26): свидетельство",
    "",
    "## Дефект", "", "текст после секции",
  ].join("\n")
  test("комменты секции: контент-хэш id, автор в userId, голова в тексте", () => {
    const parsed = parseCommentsMarkdown(MD, "DOC")
    expect(parsed.length).toBe(2)
    expect(parsed[0].id).toMatch(/^cDOC-[0-9a-f]{8}$/)
    expect(parsed[0].userId).toBe("AGENTS")
    expect(parsed[0].text).toBe("AGENTS (2026-11-05 19:43): вопрос агента")
    expect(parsed[1].id).toMatch(/^cDOC-[0-9a-f]{8}$/)
    expect(parsed[1].id).not.toBe(parsed[0].id)
  })
  test("тред-ответы несут id и userId — юзерский ответ тредом детектится", () => {
    const parsed = parseCommentsMarkdown(MD, "DOC")
    expect(parsed[0].replies.length).toBe(1)
    expect(parsed[0].replies[0].id).toMatch(/^rDOC-[0-9a-f]{8}$/)
    expect(parsed[0].replies[0].userId).toBe("Юра")
    expect(parsed[0].replies[0].text).toContain("ответ владельца")
  })
  test("без секции «Комменты» — пусто", () => {
    expect(parseCommentsMarkdown("## Прочее\nтекст", "DOC")).toEqual([])
  })
  test("трейлинг-хвост после последней ## — комменты детектятся ( High-1: CLI-аппенд в конец дока)", () => {
    const MD_TAIL = [
      "## План", "", "тело плана", "",
      "## Комменты", "", "- **AGENTS** (t0): старый",
      "", "## Дефект", "", "текст дефекта", "",
      "- **AGENTS** (2026-11-06 16:39): PR готов к ревью",
      "", "подробности вердикта", "",
      "- **AGENTS** (2026-11-06 16:46): вердикт раунд 2",
    ].join("\n")
    const parsed = parseCommentsMarkdown(MD_TAIL, "DOC")
    // старая секция + оба хвостовых пункта
    expect(parsed.length).toBe(3)
    expect(parsed[1].id).toMatch(/^cDOC-[0-9a-f]{8}$/)
    expect(parsed[1].text).toContain("PR готов к ревью")
    // продолжение блока (тело вердикта) входит в text пункта
    expect(parsed[1].text).toContain("подробности вердикта")
    expect(parsed[2].text).toContain("вердикт раунд 2")
  })
  test("юзерский тред-ответ в хвосте (вложенный пункт с датой) — reply последнего пункта", () => {
    const MD_TAIL_REPLY = "## Секции\n\nконтент\n\n- **AGENTS** (2026-11-06 16:46): вопрос\n  - **Юра** (2026-11-06 17:00): ответ владельца\n"
    const parsed = parseCommentsMarkdown(MD_TAIL_REPLY, "DOC")
    expect(parsed.length).toBe(1)
    expect(parsed[0].replies[0].userId).toBe("Юра")
    expect(parsed[0].replies[0].text).toContain("ответ владельца")
  })
  test("контентные абзацы хвоста ДО первого пункта не теряются", () => {
    const MD_LEAD = "## Отчёт\n\nабзац отчёта\n\n- **AGENTS** (2026-11-06 16:46): коммент\n"
    const parsed = parseCommentsMarkdown(MD_LEAD, "DOC")
    expect(parsed.length).toBe(1)
    expect(parsed[0].text).not.toContain("абзац отчёта")
  })
  test("id контент-стабильны: снос верхнего коммента остальные id не меняет", () => {
    const MD_FULL = [
      "- **Юра** (2026-11-07 09:00): первый вопрос",
      "- **AGENTS** (2026-11-07 09:05): ответ агента",
      "- **Юра** (2026-11-07 09:10): уточнение",
    ].join("\n")
    const before = parseCommentsMarkdown(MD_FULL, "DOC")
    const MD_TOP_DELETED = MD_FULL.split("\n").slice(1).join("\n")
    const after = parseCommentsMarkdown(MD_TOP_DELETED, "DOC")
    expect(after.map((c) => c.id)).toEqual(before.slice(1).map((c) => c.id))
    // формат: префикс + контент-хэш (hex), не позиционный индекс
    for (const c of after) expect(c.id).toMatch(/^cDOC-[0-9a-f]{8}$/)
  })
  test("идентичные replies — один хэш на контент, дубль не зацикливает будильник", () => {
    const reply = "  - **Юра** (2026-11-07 09:00): дублированный ответ"
    const MD_DUP_REPLY = [
      "- **AGENTS** (2026-11-07 08:55): вопрос агента",
      reply,
      reply,
    ].join("\n")
    const parsed = parseCommentsMarkdown(MD_DUP_REPLY, "DOC")
    expect(parsed[0].replies.length).toBe(2)
    expect(parsed[0].replies[0].id).toBe(parsed[0].replies[1].id)
    const comments: TaskComment[] = parsed.flatMap((c) => [
      { id: c.id, text: c.text, createdAt: 1, userId: c.userId },
      ...c.replies.map((r) => ({ id: r.id, text: r.text, createdAt: 2, userId: r.userId })),
    ])
    // lastSeen по первому дублю: свежих быть не должно — second копия уже
    // «прожита» (findIndex по первому вхождению вернул бы будку на неё вновь)
    const fresh = newUserComments(comments, { at: 0, id: parsed[0].replies[0].id }, "BOT")
    expect(fresh).toEqual([])
  })
})

describe("newUserComments — свежесть после сноса коммента (сдвиг позиционных id)", () => {
  const line = (user: string, ts: string, text: string): string => `- **${user}** (${ts}): ${text}`
  const toTaskComments = (md: string): TaskComment[] =>
    parseCommentsMarkdown(md, "DOC").map((c, i) => ({
      id: c.id,
      text: c.text,
      createdAt: i + 1,
      userId: c.userId,
    }))

  test("снос обработанного коммента сверху не подменяет lastSeen и не теряет свежий", () => {
    const MD_THREE = [
      line("Юра", "2026-11-07 09:00", "старый обработанный"),
      line("Юра", "2026-11-07 09:05", "последний обработанный"),
      line("Юра", "2026-11-07 09:10", "новый после последнего захода"),
    ].join("\n")
    const seen = toTaskComments(MD_THREE)
    const lastSeen = { at: 3, id: seen[2].id }
    expect(newUserComments(seen, lastSeen, "BOT")).toEqual([])

    const MD_TOP_DELETED = MD_THREE.split("\n").slice(1).join("\n")
    const remaining = toTaskComments(MD_TOP_DELETED)
    // после сноса id оставшихся не сдвинулись: свежих нет, дублей будки нет
    expect(newUserComments(remaining, lastSeen, "BOT")).toEqual([])
  })

  test("снос коммента ВЫШЕ lastSeen не глотает свежий: сдвиг позиционных id подменял матч", () => {
    const MD_THREE = [
      line("Юра", "2026-11-07 09:00", "старый обработанный"),
      line("Юра", "2026-11-07 09:05", "последний обработанный"),
      line("Юра", "2026-11-07 09:10", "новый после последнего захода"),
    ].join("\n")
    const seen = toTaskComments(MD_THREE)
    const lastSeen = { at: 2, id: seen[1].id }
    // свежий C не заходил: на полном наборе он и есть свежий
    expect(newUserComments(seen, lastSeen, "BOT").length).toBe(1)

    const MD_TOP_DELETED = MD_THREE.split("\n").slice(1).join("\n")
    const remaining = toTaskComments(MD_TOP_DELETED)
    const fresh = newUserComments(remaining, lastSeen, "BOT")
    expect(fresh.length).toBe(1)
    expect(fresh[0].text).toContain("новый после последнего захода")
  })
})
