// qlty-review.test.ts — юнит-тесты ядра машинного ревьюера (bun test, без
// реального git/qlty: SARIF-фикстуры из живого прогона qlty 0.661.0,
// git/qlty подменяются ReviewDeps). Контракт SARIF — константа
// SARIF_CONTRACT_FIELDS; нехватка поля = громкая ошибка, не пустой отчёт.
import { describe, expect, test } from "bun:test"
import {
  applyFilters,
  classifyDelta,
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
})

describe("classifyDelta — multiset-сверка без line", () => {
  test("сдвиг строки не делает pre-existing «введённым»", () => {
    const current = [f({ line: 42 })]
    const baseline = [f({ line: 17 })]
    const { introduced, baselineMatched } = classifyDelta(current, baseline)
    expect(introduced).toHaveLength(0)
    expect(baselineMatched).toHaveLength(1)
  })

  test("новый ключ → introduced", () => {
    const current = [f({ line: 42 }), f({ ruleId: "ruff:F841", line: 50, message: "Local variable `z` is assigned to but never used" })]
    const baseline = [f({ line: 17 })]
    const { introduced, baselineMatched } = classifyDelta(current, baseline)
    expect(introduced).toHaveLength(1)
    expect(introduced[0].ruleId).toBe("ruff:F841")
    expect(baselineMatched).toHaveLength(1)
  })

  test("count-разница: второй дубль того же ключа → introduced", () => {
    const current = [f({ line: 1 }), f({ line: 9 })]
    const baseline = [f({ line: 3 })]
    const { introduced, baselineMatched } = classifyDelta(current, baseline)
    expect(introduced).toHaveLength(1)
    expect(introduced[0].line).toBe(9)
    expect(baselineMatched).toHaveLength(1)
  })

  test("разные файлы с одинаковым message — разные ключи", () => {
    const current = [f({ path: "src/other.py", line: 1 })]
    const baseline = [f({ path: "src/a.py", line: 1 })]
    const { introduced, baselineMatched } = classifyDelta(current, baseline)
    expect(introduced).toHaveLength(1)
    expect(baselineMatched).toHaveLength(0)
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
      introduced: [f({ ruleId: "ruff:F401", path: "src/new.py", line: 3 })],
      baselineMatched: [f({ ruleId: "clippy:needless_range_loop", path: "cli/src/old.rs", line: 88, level: "warning", message: "this for-loop..." })],
      smells: [f({ ruleId: "qlty:similar-code", path: "src/x.py", line: 10, level: "warning", message: "Found 16 lines of similar code in 2 locations (mass = 53)" })],
    })
    expect(md).toContain("**Машинный ревью (qlty v0.661.0)** — scope `diff`")
    expect(md).toContain("**Введено веткой (delta)**")
    expect(md).toContain("- [ruff:F401] src/new.py:3 (warning) — `os` imported but unused")
    expect(md).toContain("**Baseline (pre-existing, не от этой ветки)**")
    expect(md).toContain("- [clippy:needless_range_loop] cli/src/old.rs:88")
    expect(md).toContain("**Smells (qlty smells, новые против upstream)**")
    expect(md).toContain("Итог: 1 введено + 1 baseline + 1 smells.")
  })

  test("чистый прогон — без групп, ✅-итог", () => {
    const md = renderReport({
      qltyVersion: "0.661.0",
      scope: "diff",
      upstream: "origin/master",
      directory: "/repo",
      notes: [],
      introduced: [],
      baselineMatched: [],
      smells: [],
    })
    expect(md).toContain("✅ новых машинных находок нет (baseline: 0)")
    expect(md).not.toContain("**Введено веткой")
  })

  test("notes идут предупреждениями в шапку", () => {
    const md = renderReport({
      qltyVersion: "0.661.0",
      scope: "commit",
      upstream: "origin/master",
      directory: "/repo",
      commit: "abc123",
      notes: ["scope=commit: прогон по рабочей копии."],
      introduced: [],
      baselineMatched: [],
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
  /** SARIF по cwd: текущий репо vs baseline-ворктри */
  qltyByCwd: (cwd: string, args: string[]) => string
  tmpWorktrees: string[]
}

function fakeDeps(repo: FakeRepo): ReviewDeps {
  return {
    git: async (args: string[], _cwd: string) => {
      const head = args[0]
      if (head === "worktree" && args[1] === "add") {
        repo.tmpWorktrees.push(args[3])
        return ""
      }
      if (head === "worktree" && args[1] === "remove") return ""
      const key = args.join(" ")
      if (!(key in repo.git)) throw new Error(`fake git: нет сценария для «${key}»`)
      return repo.git[key]
    },
    qlty: async (args: string[], cwd: string) => repo.qltyByCwd(cwd, args),
  }
}

const CURRENT_SARIF = JSON.stringify({
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

const BASELINE_SARIF = JSON.stringify({
  runs: [
    {
      tool: { driver: { semanticVersion: "0.661.0" } },
      results: [
        {
          ruleId: "clippy:needless_return",
          level: "warning",
          message: { text: "unneeded `return` statement" },
          locations: [{ physicalLocation: { artifactLocation: { uri: "src/changed.py" }, region: { startLine: 7 } } }],
        },
      ],
    },
  ],
})

const SMELLS_EMPTY = JSON.stringify({ runs: [{ tool: { driver: { semanticVersion: "0.661.0" } }, results: [] }] })

describe("runReview — оркестрация на fake-deps", () => {
  test("diff: delta-классификация против baseline-ворктри на merge-base", async () => {
    const repo: FakeRepo = {
      git: {
        "merge-base origin/master HEAD": "abcbase123",
        "diff --name-only --diff-filter=d abcbase123": "src/changed.py\n",
        "ls-files --others --exclude-standard": "src/untracked.py\n",
        "ls-tree -r --name-only abcbase123": "src/changed.py\n",
      },
      qltyByCwd: (cwd, args) => {
        if (args[0] === "smells") return SMELLS_EMPTY
        return cwd.includes("qlty-review-baseline") ? BASELINE_SARIF : CURRENT_SARIF
      },
      tmpWorktrees: [],
    }
    const r = await runReview(
      { scope: "diff", directory: "/repo", upstream: "origin/master" },
      fakeDeps(repo),
    )
    // needless_return есть и в baseline (строка сдвинулась 7→9) → baseline;
    // F401 — только в current → введено; untracked в путях прогона
    expect(r.introduced).toBe(1)
    expect(r.baseline).toBe(1)
    expect(r.smells).toBe(0)
    expect(r.markdown).toContain("[ruff:F401] src/changed.py:1")
    expect(r.markdown).toContain("Baseline (pre-existing")
    expect(repo.tmpWorktrees).toHaveLength(1)
  })

  test("commit: пути из diff-tree + note в отчёте", async () => {
    const repo: FakeRepo = {
      git: {
        "merge-base origin/master HEAD": "abcbase123",
        "diff-tree --no-commit-id --name-only -r feedbac4": "src/changed.py\n",
        "ls-tree -r --name-only abcbase123": "src/changed.py\n",
      },
      qltyByCwd: (_cwd, args) => (args[0] === "smells" ? SMELLS_EMPTY : CURRENT_SARIF),
      tmpWorktrees: [],
    }
    const r = await runReview(
      { scope: "commit", directory: "/repo", commit: "feedbac4" },
      fakeDeps(repo),
    )
    expect(r.markdown).toContain("scope `commit` (`feedbac4`)")
    expect(r.markdown).toContain("> ⚠ scope=commit: прогон по рабочей копии")
  })

  test("новые файлы не попадают в baseline-пробу", async () => {
    const repo: FakeRepo = {
      git: {
        "merge-base origin/master HEAD": "abcbase123",
        "diff --name-only --diff-filter=d abcbase123": "src/new_file.py\n",
        "ls-files --others --exclude-standard": "",
        "ls-tree -r --name-only abcbase123": "src/other.py\n",
      },
      qltyByCwd: (cwd, args) => {
        if (args[0] === "smells") return SMELLS_EMPTY
        if (cwd.includes("qlty-review-baseline")) throw new Error("baseline-прогон не должен был случиться: путь не существует на base")
        return CURRENT_SARIF
      },
      tmpWorktrees: [],
    }
    const r = await runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo))
    // baseline-прогон пропущен (basePaths пуст) → всё current идёт в introduced
    expect(r.introduced).toBe(2)
    expect(r.baseline).toBe(0)
  })

  test("пустой merge-base — громкая ошибка", async () => {
    const repo: FakeRepo = {
      git: { "merge-base origin/master HEAD": "" },
      qltyByCwd: () => "",
      tmpWorktrees: [],
    }
    expect(
      runReview({ scope: "diff", directory: "/repo" }, fakeDeps(repo)),
    ).rejects.toThrow(/merge-base.*пуст/)
  })
})
