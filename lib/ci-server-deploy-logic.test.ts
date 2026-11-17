// ci-server-deploy-logic.test.ts — исполнительная проверка логики server_deploy
// из .github/workflows/ci.yml (задача uwuwu#1211). Пин ci-deploy-contract.test.ts
// защищает текст воркфлоу; этот тест защищает поведение: shell-скрипт шага
// «Diff → области» извлекается из ci.yml и исполняется в git-песочнице на
// реальных диффах. Пин поймает удаление блока, этот тест — сломанную логику
// внутри (кривой case-паттерн, гард форс-пуша, дифф-база). Сети нет: только
// локальный git + bash.
import { describe, expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync, mkdirSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const CI_PATH = join(import.meta.dir, "..", ".github", "workflows", "ci.yml")

/** Тело run-блока шага «Diff → области» (единственного с id: areas). */
function areasScript(src: string): string {
  const nameIdx = src.indexOf("- name: Diff → области")
  if (nameIdx === -1) return ""
  const runIdx = src.indexOf("run: |", nameIdx)
  if (runIdx === -1) return ""
  const lines = src.slice(runIdx + "run: |".length).split("\n")
  const body: string[] = []
  for (const line of lines.slice(1)) {
    if (line.trim() === "") {
      body.push("")
      continue
    }
    if (/^ {10,}/.test(line)) body.push(line.slice(10))
    else break
  }
  return body.join("\n")
}

const src = readFileSync(CI_PATH, "utf8")
const script = areasScript(src)

/** Git-песочница: репо с базовым коммитом и origin/master-указателем на него. */
function sandbox(): string {
  const dir = mkdtempSync(join(tmpdir(), "ci-deploy-logic-"))
  const git = (args: string[]): string => {
    const r = spawnSync("git", args, { cwd: dir, stdio: "pipe" })
    if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.toString()}`)
    return r.stdout.toString()
  }
  git(["init", "-b", "master"])
  git(["config", "user.email", "test@test"])
  git(["config", "user.name", "test"])
  for (const p of [
    "server/crates/demo/src/lib.rs",
    "server/Cargo.toml",
    "server/Dockerfile",
    "server/config.example.toml",
    "server/ops/relay/demo.sh",
    "lib/demo.ts",
    "README.md",
  ]) {
    mkdirSync(join(dir, p, ".."), { recursive: true })
    writeFileSync(join(dir, p), "base\n")
  }
  git(["add", "-A"])
  git(["commit", "-m", "base"])
  git(["update-ref", "refs/remotes/origin/master", "HEAD"])
  return dir
}

function gitHead(dir: string): string {
  const r = spawnSync("git", ["rev-parse", "HEAD"], { cwd: dir, stdio: "pipe" })
  if (r.status !== 0) throw new Error(`rev-parse: ${r.stderr.toString()}`)
  return r.stdout.toString().trim()
}

/** Коммит одной правки поверх HEAD; возвращает SHA родителя (база пуша). */
function commit(dir: string, path: string, content: string): { base: string } {
  const base = gitHead(dir)
  const p = join(dir, path)
  mkdirSync(p.slice(0, p.lastIndexOf("/")), { recursive: true })
  writeFileSync(p, content)
  const add = spawnSync("git", ["add", "-A"], { cwd: dir, stdio: "pipe" })
  if (add.status !== 0) throw new Error(`git add: ${add.stderr.toString()}`)
  const c = spawnSync("git", ["commit", "-m", `touch ${path}`], { cwd: dir, stdio: "pipe" })
  if (c.status !== 0) throw new Error(`git commit: ${c.stderr.toString()}`)
  return { base }
}

/** Прогон скрипта шага с заданным событием; возвращает outputs (GITHUB_OUTPUT). */
function runStep(dir: string, eventName: string, pushBase: string): Record<string, string> {
  const outFile = join(dir, "github_output")
  writeFileSync(outFile, "")
  const r = spawnSync("bash", ["-c", script], {
    cwd: dir,
    env: { ...process.env, GITHUB_EVENT_NAME: eventName, PUSH_BASE: pushBase, GITHUB_OUTPUT: outFile },
    stdio: "pipe",
  })
  if (r.status !== 0) throw new Error(`step failed: ${r.stderr.toString()}`)
  const outputs: Record<string, string> = {}
  for (const line of readFileSync(outFile, "utf8").split("\n")) {
    const m = line.match(/^(.+)=(.*)$/)
    if (m) outputs[m[1]] = m[2]
  }
  return outputs
}

/** Полный нулевой SHA из воркфлоу (гард форс-пуша). */
const ZERO = "0000000000000000000000000000000000000000"

describe("ci.yml changes — логика server_deploy исполняется (git-песочница)", () => {
  test("шаг «Diff → области» извлекается из ci.yml — носитель логики жив", () => {
    expect(script).toContain("server_deploy=false")
    expect(script).toContain('GITHUB_EVENT_NAME" = "push"')
  })

  test("push: правка server/crates/** → server_deploy=true (главный деплой-триггер)", () => {
    const dir = sandbox()
    try {
      const { base } = commit(dir, "server/crates/demo/src/lib.rs", "changed\n")
      const out = runStep(dir, "push", base)
      expect(out.server_deploy).toBe("true")
      expect(out.rust).toBe("true") // push-ран: run_all — rust всегда
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: правка server/Dockerfile → true; вне входов образа → false", () => {
    const dir = sandbox()
    try {
      let base = commit(dir, "server/Dockerfile", "FROM base\n").base
      expect(runStep(dir, "push", base).server_deploy).toBe("true")
      base = commit(dir, "lib/demo.ts", "changed\n").base
      expect(runStep(dir, "push", base).server_deploy).toBe("false")
      base = commit(dir, "README.md", "changed\n").base
      expect(runStep(dir, "push", base).server_deploy).toBe("false")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: правка server/ops/** → false — релей не деплоит uwuwu-server (развязка #1202)", () => {
    const dir = sandbox()
    try {
      const { base } = commit(dir, "server/ops/relay/demo.sh", "changed\n")
      expect(runStep(dir, "push", base).server_deploy).toBe("false")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: форс-пуш (нулевой before) → фоллбек HEAD^ находит серверную правку", () => {
    const dir = sandbox()
    try {
      commit(dir, "server/crates/demo/src/lib.rs", "forced\n")
      expect(runStep(dir, "push", ZERO).server_deploy).toBe("true")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: форс-пуш без серверных правок → false (фоллбек не расширяет фильтр)", () => {
    const dir = sandbox()
    try {
      commit(dir, "lib/demo.ts", "forced\n")
      expect(runStep(dir, "push", ZERO).server_deploy).toBe("false")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: недоступный в истории SHA → гард уводит базу на HEAD^", () => {
    const dir = sandbox()
    try {
      commit(dir, "server/Cargo.toml", "[workspace]\n")
      expect(runStep(dir, "push", "f".repeat(40)).server_deploy).toBe("true")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("pull_request: даже при серверной правке server_deploy=false — деплой не на PR", () => {
    const dir = sandbox()
    try {
      commit(dir, "server/crates/demo/src/lib.rs", "pr-change\n")
      const out = runStep(dir, "pull_request", "")
      expect(out.server_deploy).toBe("false")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("push: пустой before (первый пуш ветки) → фоллбек HEAD^, деплой по последнему коммиту", () => {
    const dir = sandbox()
    try {
      commit(dir, "server/config.example.toml", "changed\n")
      expect(runStep(dir, "push", "").server_deploy).toBe("true")
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })
})
