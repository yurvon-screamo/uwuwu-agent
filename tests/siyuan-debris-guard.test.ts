// siyuan-debris-guard.test.ts — гвард «SiYuan вырезан из исходного кода
// полностью» (критерий приёмки задачи 317), bun test, без сети.
//
// Слепое пятно критерия: приёмка верифицировалась `git grep -i siyuan`,
// который не ловит SiYuan-эндпоинты (appendBlock/lsNotebooks/exportMdContent/
// /api/query/sql) — имя API слово «siyuan» не содержит. Гвард закрывает оба
// класса в коде (.ts/.rs/.py): строки siyuan/wiki_ops + эндпоинт-маркеры.
// docs/** и *.md исключены: исторические упоминания в вики и ADR — архив
// по определению (отчёт инженера задачи 317).
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join } from "node:path"
import { describe, expect, test } from "bun:test"

const ROOT = join(import.meta.dir, "..")
const SELF = join(import.meta.dir, "siyuan-debris-guard.test.ts")

const CODE_EXT = new Set([".ts", ".rs", ".py"])
const EXCLUDE_DIRS = new Set([
  "node_modules", "target", ".git", "docs", "hub-clone", "dist", ".worktrees", "wt",
])

// /api/query/sql в регулярке через [/] — не рвать комментарий-фенс
const SIYUAN_MARKERS = [
  /siyuan/i,
  /wiki_ops/,
  /appendBlock/,
  /lsNotebooks/,
  /exportMdContent/,
  /[/]api[/]query[/]sql/,
] as const

function codeFiles(dir: string, acc: string[] = []): string[] {
  let entries: string[]
  try {
    entries = readdirSync(dir)
  } catch {
    return acc
  }
  for (const name of entries) {
    const full: string = join(dir, name)
    const st = (() => {
      try {
        return statSync(full)
      } catch {
        return null
      }
    })()
    if (!st) continue
    if (st.isDirectory()) {
      if (!EXCLUDE_DIRS.has(name)) codeFiles(full, acc)
      continue
    }
    if (CODE_EXT.has(extOf(name))) acc.push(full)
  }
  return acc
}

function extOf(name: string): string {
  const i = name.lastIndexOf(".")
  return i === -1 ? "" : name.slice(i)
}

function full(p: string): string {
  try {
    return require("node:fs").realpathSync(p)
  } catch {
    return p
  }
}

describe("SiYuan-debris guard — код без SiYuan-строк (критерий задачи 317)", () => {
  test("в коде (.ts/.rs/.py) нет siyuan/wiki_ops/эндпоинтов SiYuan", () => {
    const offenders: Array<{ file: string; hit: string }> = []
    for (const file of codeFiles(ROOT)) {
      if (full(file) === full(SELF)) continue // сам гвард несёт маркеры-паттерны
      const content = readFileSync(file, "utf8")
      for (const marker of SIYUAN_MARKERS) {
        if (marker.test(content)) offenders.push({ file, hit: String(marker) })
      }
    }
    expect(
      offenders.map((o) => `${o.file.replace(`${ROOT}/`, "")} ← ${o.hit}`),
    ).toEqual([])
  })
})
