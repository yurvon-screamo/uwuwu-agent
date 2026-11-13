// autodone.test.ts — юнит-тесты чистых функций автодона (20261112160316).
import { describe, expect, test } from "bun:test"
import {
  prCarrierOf,
  giteaRepoOf,
  githubProbeVerdict,
  giteaProbeVerdict,
  autodoneAction,
} from "./autodone"

describe("prCarrierOf", () => {
  test("github PR url распознаётся по /pull/<n>", () => {
    const c = prCarrierOf("https://github.com/yurvon-screamo/uwuwu-agent/pull/232", "", "")
    expect(c).toEqual({
      kind: "github",
      url: "https://github.com/yurvon-screamo/uwuwu-agent/pull/232",
    })
  })

  test("gitea PR url распознаётся по /pulls/<n> — host/index/slug", () => {
    const c = prCarrierOf("https://git.l4a.ru/ek/ai-generator/pulls/84", "", "")
    expect(c).toEqual({
      kind: "gitea",
      url: "https://git.l4a.ru/ek/ai-generator/pulls/84",
      host: "git.l4a.ru",
      index: "84",
      slug: "ek/ai-generator",
    })
  })

  test("wiki-URL (docs-only канон) — носитель «без ПР»", () => {
    const c = prCarrierOf(
      "https://github.com/yurvon-screamo/uwuwu-hub/issues/1",
      "work/task-123", "yurvon-screamo/uwuwu-agent",
    )
    expect(c).toEqual({ kind: "none" })
  })

  test("пустая ячейка + ветка + github-репо — фоллбек по ветке", () => {
    const c = prCarrierOf("", "work/task-20261112160316", "yurvon-screamo/uwuwu-agent")
    expect(c).toEqual({ kind: "github-branch", branch: "work/task-20261112160316" })
  })

  test("пустая ячейка + ветка + gitea-репо (хост с точкой) — gitea-фоллбек", () => {
    const c = prCarrierOf("", "work/task-x", "git.l4a.ru/ek/ai-generator")
    expect(c).toEqual({
      kind: "gitea-branch",
      branch: "work/task-x",
      host: "git.l4a.ru",
      slug: "ek/ai-generator",
    })
  })

  test("пустая ячейка без ветки — «без ПР»", () => {
    expect(prCarrierOf("", "", "yurvon-screamo/uwuwu-agent")).toEqual({ kind: "none" })
  })

  test("PR-паттерн без схемы — invalid (не бросает, ревью Common-1)", () => {
    expect(prCarrierOf("git.l4a.ru/ek/ai-generator/pulls/84", "", ""))
      .toEqual({ kind: "invalid", url: "git.l4a.ru/ek/ai-generator/pulls/84" })
    expect(prCarrierOf("github.com/o/r/pull/9", "", ""))
      .toEqual({ kind: "invalid", url: "github.com/o/r/pull/9" })
  })

  test("PR-паттерн со схемой, но пустым slug — invalid", () => {
    expect(prCarrierOf("https://git.l4a.ru/pulls/84", "", "")).toEqual({
      kind: "invalid",
      url: "https://git.l4a.ru/pulls/84",
    })
  })
})

describe("giteaRepoOf", () => {
  test("хост с точкой в первом сегменте — gitea", () => {
    expect(giteaRepoOf("git.l4a.ru/ek/ai-generator")).toEqual({
      host: "git.l4a.ru",
      slug: "ek/ai-generator",
    })
  })

  test("owner/name без хоста — github (null)", () => {
    expect(giteaRepoOf("yurvon-screamo/uwuwu-agent")).toBeNull()
  })
})

describe("githubProbeVerdict", () => {
  test("MERGED → merged", () => {
    expect(githubProbeVerdict({ state: "MERGED", mergeable: "MERGEABLE", statusCheckRollup: [] }))
      .toBe("merged")
  })

  test("не-OPEN (CLOSED) → unknown", () => {
    expect(githubProbeVerdict({ state: "CLOSED", statusCheckRollup: [] })).toBe("unknown")
  })

  test("CONFLICTING / DIRTY → conflict", () => {
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "CONFLICTING", statusCheckRollup: [] }))
      .toBe("conflict")
    expect(githubProbeVerdict({ state: "OPEN", mergeStateStatus: "DIRTY", statusCheckRollup: [] }))
      .toBe("conflict")
  })
  test("упавший required-чек → checks-failed (обе формы чеков)", () => {
    const rollup = [
      { status: "COMPLETED", conclusion: "SUCCESS" },
      { status: "COMPLETED", conclusion: "FAILURE" },
    ]
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "MERGEABLE", statusCheckRollup: rollup }))
      .toBe("checks-failed")
    // StatusContext-форма: state FAILURE
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "MERGEABLE", statusCheckRollup: [{ state: "FAILURE" }] }))
      .toBe("checks-failed")
  })

  test("checks в полёте → checks-pending (обе формы чеков)", () => {
    expect(githubProbeVerdict({
      state: "OPEN",
      mergeable: "MERGEABLE",
      statusCheckRollup: [{ status: "IN_PROGRESS", conclusion: null }],
    })).toBe("checks-pending")
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "MERGEABLE", statusCheckRollup: [{ state: "PENDING" }] }))
      .toBe("checks-pending")
  })

  test("зелёный rollup → clean; UNSTABLE (не-обязательные упавшие) → clean", () => {
    // живая форма CheckRun (gh pr view): status COMPLETED + conclusion SUCCESS
    const ok = [{ status: "COMPLETED", conclusion: "SUCCESS" }]
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "MERGEABLE", statusCheckRollup: ok }))
      .toBe("clean")
    expect(githubProbeVerdict({ state: "OPEN", mergeStateStatus: "UNSTABLE", statusCheckRollup: ok }))
      .toBe("clean")
    // StatusContext-форма
    expect(githubProbeVerdict({ state: "OPEN", mergeable: "MERGEABLE", statusCheckRollup: [{ state: "SUCCESS" }] }))
      .toBe("clean")
  })
})

describe("giteaProbeVerdict", () => {
  test("merged → merged; не-open → unknown", () => {
    expect(giteaProbeVerdict({ state: "open", merged: true, mergeable: true }, "")).toBe("merged")
    expect(giteaProbeVerdict({ state: "closed", merged: false, mergeable: true }, "success"))
      .toBe("unknown")
  })

  test("mergeable=false → conflict; ci failure → checks-failed", () => {
    expect(giteaProbeVerdict({ state: "open", mergeable: false }, "success")).toBe("conflict")
    expect(giteaProbeVerdict({ state: "open", mergeable: true }, "failure")).toBe("checks-failed")
  })

  test("ci pending/warning → checks-pending; пустой ci (не настроен) → clean", () => {
    expect(giteaProbeVerdict({ state: "open", mergeable: true }, "pending")).toBe("checks-pending")
    expect(giteaProbeVerdict({ state: "open", mergeable: true }, "warning")).toBe("checks-pending")
    expect(giteaProbeVerdict({ state: "open", mergeable: true }, "")).toBe("clean")
  })
})

describe("autodoneAction", () => {
  const base = { pendingMs: 0, timeoutMs: 60 * 60_000, unknownStreak: 0, unknownMax: 3 }

  test("clean → merge; conflict/failed → bounce с причиной", () => {
    expect(autodoneAction("clean", base)).toEqual({ t: "merge" })
    expect(autodoneAction("conflict", base)).toMatchObject({ t: "bounce" })
    expect(autodoneAction("checks-failed", base)).toMatchObject({ t: "bounce" })
  })

  test("merged → done; pending до таймаута → wait, после — escalate", () => {
    expect(autodoneAction("merged", base)).toMatchObject({ t: "done" })
    expect(autodoneAction("checks-pending", { ...base, pendingMs: 59 * 60_000 })).toEqual({ t: "wait" })
    expect(autodoneAction("checks-pending", { ...base, pendingMs: 61 * 60_000 })).toMatchObject({ t: "escalate" })
  })

  test("unknown до предела streak → wait, на пределе — escalate", () => {
    expect(autodoneAction("unknown", { ...base, unknownStreak: 1 })).toEqual({ t: "wait" })
    expect(autodoneAction("unknown", { ...base, unknownStreak: 2 })).toMatchObject({ t: "escalate" })
  })
})
