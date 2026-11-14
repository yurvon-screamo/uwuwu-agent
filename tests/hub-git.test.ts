// tests/hub-git.test.ts — git-чтение wiki из дерева origin/main (hub-git):
// show/exists/listDir на фикстурном клоне с origin/main-рефом; fetch-гейт
// отключён env-ом (отрицательный интервал) — тесты без сети.
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

process.env.UWUWU_HUB_CLONE_REFRESH_SEC = "-1" // до импорта: константа печётся на импорте
process.env.UWUWU_HUB_REPO = "test/hub-git-fixture"

import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test"
import { hubGitExists, hubGitListDir, hubGitShow, resetHubGitCacheForTests } from "../lib/hub-git"

function git(dir: string, args: string[]): void {
  const r = Bun.spawnSync(["git", "-C", dir, ...args], { stdout: "pipe", stderr: "pipe" })
  if (r.exitCode !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.toString().slice(0, 200)}`)
}

let fixture = ""
let fixtureBase = ""

beforeAll(() => {
  // UWUWU_HUB_CLONE_ROOT — база клона: hubCloneRoot аппендит «<repo>»-сегмент
  fixtureBase = mkdtempSync(join(tmpdir(), "hub-git-test-"))
  fixture = join(fixtureBase, "test__hub-git-fixture")
  mkdirSync(fixture, { recursive: true })
  git(fixture, ["init", "-q", "-b", "main"])
  mkdirSync(join(fixture, "docs/Projects/uwuwu"), { recursive: true })
  writeFileSync(join(fixture, "docs/Projects/uwuwu/Node-pinning.md"), "---\ntitle: Node-pinning\n---\n\n## Фабрики\n- kisa: [uwuwu]\n")
  writeFileSync(join(fixture, "docs/Projects/uwuwu/Фабрика: сиквенс.md"), "контент с пробелами в имени\n")
  mkdirSync(join(fixture, "docs/Projects/uwuwu/ADR"), { recursive: true })
  writeFileSync(join(fixture, "docs/Projects/uwuwu/ADR/ADR-001.md"), "# ADR-001\n")
  git(fixture, ["add", "."])
  git(fixture, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "fixture"])
  // fetch-гейт выключен: origin/main-реф строится локально (паритет скачанного дерева)
  git(fixture, ["update-ref", "refs/remotes/origin/main", "HEAD"])
  process.env.UWUWU_HUB_CLONE_ROOT = fixtureBase
  resetHubGitCacheForTests()
})

afterEach(() => {
  resetHubGitCacheForTests()
})

describe("hubGitShow / hubGitExists: дерево origin/main", () => {
  test("файл читается, exists:true", () => {
    const md = hubGitShow("docs/Projects/uwuwu/Node-pinning.md")
    expect(md?.includes("## Фабрики")).toBe(true)
    expect(hubGitExists("docs/Projects/uwuwu/Node-pinning.md")).toBe(true)
  })

  test("нет файла → show:null, exists:false (не бросают)", () => {
    expect(hubGitShow("docs/Projects/uwuwu/нет.md")).toBeNull()
    expect(hubGitExists("docs/Projects/uwuwu/нет.md")).toBe(false)
  })

  test("имя с пробелом честное (паритет readdir-чтений)", () => {
    expect(hubGitShow("docs/Projects/uwuwu/Фабрика: сиквенс.md")?.includes("пробелами")).toBe(true)
  })
})

describe("hubGitListDir: дети папки", () => {
  test("файлы и папки различаются; кириллица/пробелы без quotePath-экранирования", () => {
    const entries = hubGitListDir("docs/Projects/uwuwu/")
    const pinning = entries.find((e) => e.name === "Node-pinning.md")
    const adr = entries.find((e) => e.name === "ADR")
    const seq = entries.find((e) => e.name === "Фабрика: сиквенс.md")
    expect(pinning?.isDir).toBe(false)
    expect(adr?.isDir).toBe(true)
    expect(seq?.isDir).toBe(false)
    expect(seq?.name).toBe("Фабрика: сиквенс.md")
  })

  test("нет папки → пусто (не бросает)", () => {
    expect(hubGitListDir("docs/Projects/нет/")).toEqual([])
  })
})

afterAll(() => {
  rmSync(fixtureBase, { recursive: true, force: true })
})
