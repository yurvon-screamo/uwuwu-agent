// qlty-review.test.ts — юнит-тесты ядра машинного ревьюера (bun test, без
// реального git/qlty: SARIF-фикстуры из живого прогона qlty 0.661.0,
// git/qlty подменяются ReviewDeps). Контракт SARIF — константа
// SARIF_CONTRACT_FIELDS; нехватка поля = громкая ошибка, не пустой отчёт.
import { describe, expect, test } from "bun:test"
import {
  applyFilters,
  parseSarif,
  renderReport,
  SARIF_CONTRACT_FIELDS,
  type QltyFinding,
} from "./qlty-review-core"
import { runReview, type ReviewDeps } from "./qlty-review"

// Живой SARIF qlty 0.661.0 (qlty check --sarif на py-файле с ruff/radarlint
// находками), структурно сокращён до контрактных полей.
const SARIF_OK = JSON.stringify({
  runs: [
    {
      tool: { driver: { semanticVersion: "0.661.0" } },
      results: [
        {
          ruleId: "ruff:F401",
          level: "warning",
          message: { text: "`os` imported but unused" },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/a.py" }, region: { startLine: 1 } } }],
        },
        {
          ruleId: "ruff:F841",
          level: "warning",
          message: { text: "Local variable `y` is assigned to but never used" },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/a.py" }, region: { startLine: 5 } } }],
        },
        {
          ruleId: "radarlint-python:python:S1481",
          level: "note",
          message: { text: 'Remove the unused local variable "y".' },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/a.py" }, region: { startLine: 5 } } }],
        },
      ],
    },
  ],
})

const f = (over: Partial<QltyFinding>): QltyFinding => ({
  ruleId: "ruff:F401",
  level: "warning",
  path: "src/a.py",
  line: 1,
  message: "`os` imported but unused",
  ...over,
})

describe("parseSarif — контракт SARIF", () => {
  test("валидный SARIF → находки + версия qlty", () => {
    const { findings, qltyVersion } = parseSarif(SARIF_OK, "test")
    expect(qltyVersion).toBe("0.661.0")
    expect(findings).toHaveLength(3)
    expect(findings[0]).toEqual({
      ruleId: "ruff:F401",
      level: "warning",
      path: "src/a.py",
      line: 1,
      message: "`os` imported but unused",
    })
  })

  test("не JSON → громкая ошибка", () => {
    expect(() => parseSarif("not json at all", "test")).toThrow(/не SARIF/)
  })

  test("пустой runs → контракт нарушен", () => {
    expect(() => parseSarif(JSON.stringify({ runs: [] }), "test")).toThrow(/контракт SARIF нарушен.*runs\[0\]/)
  })

  test.each([
    ["нет semanticVersion", { runs: [{ tool: { driver: {} }, results: [] }] }],
    [
      "нет startLine у result",
      {
        runs: [
          {
            tool: { driver: { semanticVersion: "0.661.0" } },
            results: [{ ruleId: "x:1", level: "warning", message: { text: "m" }, locations: [{ physicalLocation: { artifactLocation: { uri: "a.py" } } }] }],
          },
        ],
      },
    ],
    [
      "нет ruleId у result",
      {
        runs: [
          {
            tool: { driver: { semanticVersion: "0.661.0" } },
            results: [{ level: "warning", message: { text: "m" }, locations: [{ physicalLocation: { artifactLocation: { uri: "a.py" }, region: { startLine: 1 } } }] }],
          },
        ],
      },
    ],
  ])("дрейф формата (%s) → громкая ошибка с версией контракта", (_name, broken) => {
    expect(() => parseSarif(JSON.stringify(broken), "test")).toThrow(/контракт SARIF нарушен/)
    try {
      parseSarif(JSON.stringify(broken), "test")
    } catch (e) {
      expect((e as Error).message).toContain(SARIF_CONTRACT_FIELDS[0])
    }
  })
})

describe("applyFilters — paths и level", () => {
  const findings = [
    f({ path: "cli/src/a.ts", line: 1, level: "warning" }),
    f({ path: "cli/src/deep/b.ts", line: 2, level: "note" }),
    f({ path: "docs/readme.md", line: 3, level: "warning", ruleId: "shellcheck:SC2034", message: "variable appears unused" }),
    f({ path: "factory/lib.ts", line: 4, level: "error" }),
  ]

  test("level=note — показывать всё", () => {
    expect(applyFilters(findings, { level: "note" })).toHaveLength(4)
  })

  test("дефолт low отсекает только SARIF-note (warning/error проходят)", () => {
    const kept = applyFilters(findings, {})
    expect(kept).toHaveLength(3)
    expect(kept.some((x) => x.level === "note")).toBe(false)
  })

  test("paths-глоб: * не пересекает /, ** пересекает", () => {
    expect(applyFilters(findings, { paths: ["cli/src/*.ts"] })).toHaveLength(1)
    expect(applyFilters(findings, { paths: ["cli/**/*.ts"], level: "note" })).toHaveLength(2)
    expect(applyFilters(findings, { paths: ["docs/*", "factory/*"] })).toHaveLength(2)
  })

  test("level=note + paths вместе", () => {
    expect(applyFilters(findings, { paths: ["cli/**"], level: "note" })).toHaveLength(2)
  })

  test("SARIF-error не отсекается никогда (medium/high)", () => {
    const withError = [
      f({ path: "cli/src/a.ts", level: "warning" }),
      f({ path: "cli/src/a.ts", line: 7, level: "error", ruleId: "osv-scanner:GO-2026-1234" }),
    ]
    expect(applyFilters(withError, { level: "medium" })).toHaveLength(1)
    expect(applyFilters(withError, { level: "medium" })[0].level).toBe("error")
    expect(applyFilters(withError, { level: "high" })).toHaveLength(1)
    expect(applyFilters(withError, { level: "high" })[0].level).toBe("error")
    expect(applyFilters(withError, { level: "low" })).toHaveLength(2)
  })
})

describe("renderReport — формат по мотивам qltysh", () => {
  test("группы, атрибуция [plugin:rule], итог", () => {
    const md = renderReport({
      qltyVersion: "0.661.0",
      scope: "diff",
      upstream: "origin/master",
      directory: "/repo",
      notes: [],
      delta: [f({ ruleId: "ruff:F401", path: "src/new.py", line: 3 })],
      smells: [f({ ruleId: "qlty:similar-code", path: "src/x.py", line: 10, level: "warning", message: "Found 16 lines of similar code in 2 locations (mass = 53)" })],
    })
    expect(md).toContain("**Машинный ревью (qlty v0.661.0)** — scope `diff`")
    expect(md).toContain("**Delta (qlty check --upstream: находки в изменённых строках)**")
    expect(md).toContain("- [ruff:F401] src/new.py:3 (warning) — `os` imported but unused")
    expect(md).toContain("**Smells (qlty smells, новые против upstream)**")
    expect(md).toContain("- [qlty:similar-code] src/x.py:10")
    expect(md).toContain("Итог: 1 delta + 1 smells.")
  })

  test("чистый прогон — без групп, ✅-итог", () => {
    const md = renderReport({
      qltyVersion: "0.661.0",
      scope: "diff",
      upstream: "origin/master",
      directory: "/repo",
      notes: [],
      delta: [],
      smells: [],
    })
    expect(md).toContain("✅ новых машинных находок нет.")
    expect(md).not.toContain("**Delta (")
    expect(md).not.toContain("**Smells (")
  })

  test("notes идут предупреждениями в шапку", () => {
    const md = renderReport({
      qltyVersion: "0.661.0",
      scope: "commit",
      upstream: "origin/master",
      directory: "/repo",
      commit: "abc123",
      notes: ["scope=commit: прогон по рабочей копии."],
      delta: [],
      smells: [],
    })
    expect(md).toContain("scope `commit` (`abc123`)")
    expect(md).toContain("> ⚠ scope=commit: прогон по рабочей копии.")
  })
})

// ——— runReview с fake-deps (без реального git/qlty) ———

interface FakeRepo {
  /** вывод git-команд по первому аргументу */
  git: Record<string, string>
  /** имя текущей ветки для rev-parse --abbrev-ref (дефолт work/task-x — tracked) */
  branch?: string
  /** ответ qlty по аргументам; throw в fake —qlty не должен был вызываться */
  qlty: (args: string[]) => string
  /** env, с которым qlty был вызван (для assert нейтрализации tracking) */
  qltyEnv?: Record<string, string>
}

function fakeDeps(repo: FakeRepo): ReviewDeps {
  const branch = repo.branch ?? "work/task-x"
  return {
    git: async (args: string[], _cwd: string) => {
      const key = args.join(" ")
      if (key === "rev-parse --abbrev-ref HEAD") return branch
      if (!(key in repo.git)) throw new Error(`fake git: нет сценария для «${key}»`)
      return repo.git[key]
    },
    qlty: async (args: string[], _cwd: string, env?: Record<string, string>) => {
      repo.qltyEnv = env
      return repo.qlty(args)
    },
  }
}

const DELTA_SARIF = JSON.stringify({
  runs: [
    {
      tool: { driver: { semanticVersion: "0.661.0" } },
      results: [
        {
          ruleId: "ruff:F401",
          level: "warning",
          message: { text: "`os` imported but unused" },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/changed.py" }, region: { startLine: 1 } } }],
        },
        {
          ruleId: "clippy:needless_return",
          level: "warning",
          message: { text: "unneeded `return` statement" },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/changed.py" }, region: { startLine: 9 } } }],
        },
      ],
    },
  ],
})

const SMELLS_EMPTY = JSON.stringify({ runs: [{ tool: { driver: { semanticVersion: "0.661.0" } }, results: [] }] })

// qlty в --sarif-режиме на пустой дельте отдаёт SARIF с пустыми results
// (живая проба 08.11) — текст «No modified files» только без --sarif.
const EMPTY_RESULTS_SARIF = JSON.stringify({
  runs: [{ tool: { driver: { semanticVersion: "0.661.0" } }, results: [] }],
})

const isSmellsCall = (args: string[]) => args[0] === "smells"

describe("runReview — оркестрация на fake-deps", () => {
  test("diff: строковая дельта против merge-base через qlty --upstream", async () => {
    let upstreamArg: string | undefined
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "abcbase123" },
      qlty: (args) => {
        if (isSmellsCall(args)) {
          upstreamArg = args[args.indexOf("--upstream") + 1]
          return SMELLS_EMPTY
        }
        expect(args).toContain("--upstream")
        expect(args).toContain("abcbase123")
        return DELTA_SARIF
      },
    }
    const r = await runReview({ scope: "diff", directory: "/repo", upstream: "origin/master" }, fakeDeps(repo))
    expect(r.delta).toBe(2)
    expect(r.smells).toBe(0)
    expect(r.qltyVersion).toBe("0.661.0")
    expect(r.markdown).toContain("[ruff:F401] src/changed.py:1")
    expect(r.markdown).toContain("Delta (qlty check --upstream")
    // smells — против той же базы merge-base
    expect(upstreamArg).toBe("abcbase123")
  })

  test("tracked-ветка: tracking нейтрализуется env (пост-пуш слепота qlty)", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "abcbase123" },
      branch: "work/task-xyz",
      qlty: (args) => (isSmellsCall(args) ? SMELLS_EMPTY : DELTA_SARIF),
    }
    await runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo))
    expect(repo.qltyEnv).toEqual({
      GIT_CONFIG_COUNT: "1",
      GIT_CONFIG_KEY_0: "branch.work/task-xyz.merge",
      GIT_CONFIG_VALUE_0: "",
    })
  })

  test("detached HEAD: env-нейтрализация не подставляется", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "abcbase123" },
      branch: "HEAD",
      qlty: (args) => (isSmellsCall(args) ? SMELLS_EMPTY : DELTA_SARIF),
    }
    await runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo))
    expect(repo.qltyEnv).toEqual({})
  })

  test("commit: база — родитель коммита (rev-parse ^), note в отчёте", async () => {
    const repo: FakeRepo = {
      git: { "rev-parse feedbac4^": "abcbase123" },
      qlty: (args) => {
        if (isSmellsCall(args)) return SMELLS_EMPTY
        expect(args).toContain("abcbase123")
        return DELTA_SARIF
      },
    }
    const r = await runReview({ scope: "commit", directory: "/repo", commit: "feedbac4" }, fakeDeps(repo))
    expect(r.markdown).toContain("scope `commit` (`feedbac4`)")
    expect(r.markdown).toContain("> ⚠ scope=commit: прогон по рабочей копии")
    expect(r.delta).toBe(2)
  })

  test("commit без родителя (корневой) — громкая ошибка", async () => {
    const repo: FakeRepo = {
      git: { "rev-parse rootcafe^": "" },
      qlty: () => {
        throw new Error("qlty не должен вызываться без базы")
      },
    }
    expect(
      runReview({ scope: "commit", directory: "/repo", commit: "rootcafe" }, fakeDeps(repo)),
    ).rejects.toThrow(/нет родителя/)
  })

  test("пустая дельта (SARIF с пустыми results) — ✅-отчёт", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "abcbase123" },
      qlty: (args) => (isSmellsCall(args) ? SMELLS_EMPTY : EMPTY_RESULTS_SARIF),
    }
    const r = await runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo))
    expect(r.markdown).toContain("✅ новых машинных находок нет.")
    expect(r.qltyVersion).toBe("0.661.0")
    expect(r.delta).toBe(0)
    expect(r.smells).toBe(0)
  })

  test("paths-фильтр режет дельту отчёта (считаются отфильтрованные)", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "abcbase123" },
      qlty: (args) => (isSmellsCall(args) ? SMELLS_EMPTY : DELTA_SARIF),
    }
    const r = await runReview({ scope: "diff", directory: "/repo", paths: ["src/changed.py"] }, fakeDeps(repo))
    expect(r.delta).toBe(2)
    const filtered = await runReview({ scope: "diff", directory: "/repo", paths: ["docs/*"] }, fakeDeps(repo))
    expect(filtered.delta).toBe(0)
    expect(filtered.markdown).toContain("✅ новых машинных находок нет.")
  })

  test("пустой merge-base — громкая ошибка", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "" },
      qlty: () => "",
    }
    expect(
      runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo)),
    ).rejects.toThrow(/merge-base.*пуст/)
  })
})
