// hub-cells-state.test.ts — пин проводки state в read_database_cells
// (интеграционная граница: hubTool → ghJson → gh-подпроцесс). Регрессия
// «state: issue.state» выпадет тихо: гард драфт-матча триажа деградирует
// к легаси-поведению (safe default), юниты draftRowOf останутся зелёными —
// задача 1073, слепое пятно из NOTICED инженера. Фейковый gh на PATH
// (canned JSON вместо сети) — паттерн ci-stress-workflow; ребёнок печатает
// rows, родитель сверяет state/Status по каждой строке.
import { test, expect } from "bun:test"
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"

const CHILD = join(import.meta.dir, "hub-cells-state-child.ts")
const LIB_DIR = resolve(import.meta.dir, "../lib")

const LABELS_JSON = JSON.stringify([{ name: "proj:t1073sbx" }, { name: "proj:uwuwu" }])
// fm-шапки тел: классы строк постановки 1073 — closed+Draft (мёртвый маркер),
// open+Draft (живой черновик), open+Todo (шум)
const ISSUES_JSON = JSON.stringify([
  { number: 1, title: "закрытый с мёртвым Draft-маркером", body: "---\nstatus: Draft\n---\n\nмёртвый маркер", state: "closed" },
  { number: 2, title: "живой черновик", body: "---\nstatus: Draft\n---\n\nштатный черновик", state: "open" },
  { number: 3, title: "обычная строка", body: "---\nstatus: Todo\n---\n\nне черновик", state: "open" },
])

test("read_database_cells: строка доски несёт нативный state issue — гард драфт-матча триажа жив", async () => {
  const dir = mkdtempSync(join(tmpdir(), "hub-cells-state-"))
  try {
    const binDir = join(dir, "bin")
    mkdirSync(binDir)
    const ghBin = join(binDir, "gh")
    // dispatch по URL: /issues раньше /labels — issues-вызов содержит labels
    // в query-string (issues?labels=…) и первый матч решил бы неверно
    writeFileSync(ghBin, [
      "#!/usr/bin/env bash",
      'case "$2 $3" in',
      '  *"/issues"*) cat "$FAKE_GH_ISSUES" ;;',
      '  *"/labels"*) cat "$FAKE_GH_LABELS" ;;',
      '  *) echo "fake gh: неожиданный вызов: $*" >&2; exit 1 ;;',
      "esac",
    ].join("\n"))
    chmodSync(ghBin, 0o755)
    const labelsPath = join(dir, "labels.json")
    const issuesPath = join(dir, "issues.json")
    writeFileSync(labelsPath, LABELS_JSON)
    writeFileSync(issuesPath, ISSUES_JSON)
    const out = join(dir, "rows.json")

    const proc = Bun.spawn(["bun", CHILD, LIB_DIR, out], {
      env: {
        ...process.env,
        UWUWU_HUB_REPO: "local/fake-hub",
        PATH: `${binDir}:${process.env.PATH ?? ""}`,
        FAKE_GH_LABELS: labelsPath,
        FAKE_GH_ISSUES: issuesPath,
      },
      stdout: "pipe",
      stderr: "pipe",
    })
    const [code, stderr] = await Promise.all([proc.exited, new Response(proc.stderr).text()])
    expect(stderr).toBe("")
    expect(code).toBe(0)

    const rows = JSON.parse(readFileSync(out, "utf8")) as Array<{
      rowBlockId: string
      state?: string
      cells?: Record<string, { value?: string }>
    }>
    const byId = Object.fromEntries(rows.map((r) => [String(r.rowBlockId), r]))
    // сам пин: нативный state доезжает до строки доски — выпадение проводки
    // делает значения undefined и валит тест (гард молча перестанет фильтровать)
    expect(byId["1"]?.state).toBe("closed")
    expect(byId["2"]?.state).toBe("open")
    expect(byId["3"]?.state).toBe("open")
    // контекст гарда: Status-ячейки из fm-шапок не перепутаны
    expect(byId["1"]?.cells?.Status?.value).toBe("Draft")
    expect(byId["2"]?.cells?.Status?.value).toBe("Draft")
    expect(byId["3"]?.cells?.Status?.value).toBe("Todo")
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
