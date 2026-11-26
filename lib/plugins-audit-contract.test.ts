// lib/plugins-audit-contract.test.ts — пин контракта зависимостей plugins/
// (uwuwu#1610): три moderate-адвизории в транзитивном дереве устранены
// корневыми overrides в plugins/package.json + пересобранным bun.lock.
// Статическая часть читает файлы локально, сети нет (как
// ci-deploy-contract.test.ts); живой `bun audit` — под гвардом UWUWU_E2E=1
// (культура репо: qlty-review.test.ts, screen-review.test.ts).
//
// Пинится поведение, а не точные версии: ни одна резолюция трёх пакетов не
// должна вернуться в уязвимый диапазон. Снятие override или регенерация
// lockfile со старыми копиями ломает тест; безобидный ап патченной версии —
// нет (конкретную версию не закрепляем — она со временем устареет).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const PLUGINS = join(import.meta.dir, "..", "plugins")

type Pkg = { overrides: Record<string, string> }
type Lock = { overrides: Record<string, string>; packages: Record<string, unknown> }

/** bun.lock — JSONC (trailing comma): снимаем разделители, не трогая строки. */
function readJsonc<T>(path: string): T {
  return JSON.parse(readFileSync(path, "utf8").replace(/,(\s*[}\]])/g, "$1")) as T
}

const pkg = readJsonc<Pkg>(join(PLUGINS, "package.json"))
const lock = readJsonc<Lock>(join(PLUGINS, "bun.lock"))

type Fix = { name: string; min: number[]; advisory: string }
const FIXES: Fix[] = [
  { name: "@opentelemetry/core", min: [2, 8, 0], advisory: "GHSA-8988-4f7v-96qf" },
  { name: "glob", min: [14, 0, 2], advisory: "GHSA-3g3m-2fjr-6pw4" },
  { name: "minimatch", min: [11, 0, 1], advisory: "GHSA-g3hv-75wg-m249" },
]

function atLeast(version: string, min: number[]): boolean {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version)
  if (!m) return false
  const got = [Number(m[1]), Number(m[2]), Number(m[3])]
  for (let i = 0; i < 3; i++) {
    if (got[i] !== min[i]) return got[i] > min[i]
  }
  return true
}

/** Все версии пакета, реально присутствующие в резолюции lockfile. */
function resolved(name: string): string[] {
  const out = new Set<string>()
  for (const entry of Object.values(lock.packages)) {
    const spec = Array.isArray(entry) ? (entry as unknown[])[0] : undefined
    if (typeof spec !== "string") continue
    const m = /^(.+)@(\d+\.\d+\.\d+.*)$/.exec(spec)
    if (m && m[1] === name) out.add(m[2])
  }
  return [...out].sort()
}

describe("plugins/ — резолюции транзитивных зависимостей (uwuwu#1610)", () => {
  for (const fix of FIXES) {
    test(`${fix.name}: override жив и в package.json, и в lockfile`, () => {
      expect(pkg.overrides).toHaveProperty(fix.name)
      expect(lock.overrides[fix.name]).toBe(pkg.overrides[fix.name])
    })

    test(`${fix.name}: все копии в lockfile патченые (${fix.advisory})`, () => {
      const versions = resolved(fix.name)
      expect(versions.length).toBeGreaterThan(0)
      for (const v of versions) {
        expect(atLeast(v, fix.min), `${fix.name}@${v} — уязвимая копия в резолюции (${fix.advisory})`).toBe(true)
      }
    })
  }
})

const auditE2E = process.env.UWUWU_E2E === "1"

describe("bun audit plugins/ — живой прогон (UWUWU_E2E=1)", () => {
  test.skipIf(!auditE2E)(
    "изолированная копия package.json + bun.lock → 0 уязвимостей",
    async () => {
      const { execFileSync } = await import("node:child_process")
      const { copyFileSync, mkdtempSync, rmSync } = await import("node:fs")
      const { tmpdir } = await import("node:os")

      // Фикстура из двух файлов дельты: аудит читает lockfile, node_modules не нужен.
      const dir = mkdtempSync(join(tmpdir(), "plugins-audit-e2e-"))
      try {
        for (const f of ["package.json", "bun.lock"]) copyFileSync(join(PLUGINS, f), join(dir, f))
        const out = execFileSync("bun", ["audit"], {
          cwd: dir,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "pipe"],
          timeout: 120_000,
        })
        expect(out).toContain("No vulnerabilities found")
        expect(out).not.toContain("vulnerabilities (")
      } finally {
        rmSync(dir, { recursive: true, force: true })
      }
    },
    120_000,
  )
})
