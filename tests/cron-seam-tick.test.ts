// tests/cron-seam-tick.test.ts — интеграция hub-шва крона end-to-end (инцидент
// 1110): контракты issueRows/issueAttrs юнитами покрыты (tests/platform-hub-
// tools.test.ts), но регрессия миграции была ИНТЕГРАЦИОННОЙ — шов и потребитель
// разошлись молча, каждый тик, каждый мастер. Здесь полный tickBoard против
// stub-gh: rows с linkedDocId/dateMs и attrs с алиасами custom-* обязаны
// доезжать до решений крона (файр-решение разового, reconcile клеточного
// гейта, отказ кривого расписания), а не скипаться молча до лог-точек.
// Граница — spawnSync("gh") с PATH-резолвом: stub-gh на PATH отвечает
// фикстурами issues; неожиданный вызов — exit 1 (ребёнок падает громко).
// DRY_RUN ребёнка: ноль записей/спавнов (терминальная точка файр-пути —
// лог-строка «ран-карточка и сессия не созданы»).
import { afterAll, describe, expect, test } from "bun:test"
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "cron-seam-child.ts")

interface FixtureIssue {
  number: number
  title: string
  state: string
  body: string
}

// 137 — главный путь: Вейт + разовый «04:30» (due в фиксированный now тика);
// 440 — reconcile: Вейт + cron «0 5 * * *», prevFire в state ребёнка;
// 114 — отказ: Вейт + некросс-расписание; 555/556 — тихие скипы (не Вейт /
// Вейт без schedule): их тайтлы в логе = регрессия гейта.
const FIXTURES: FixtureIssue[] = [
  {
    number: 137, title: "Lead: ночной аудит uwuwu", state: "open",
    body: "---\nstatus: Вейт\nschedule: 04:30\nautodone: true\n---\n## Инструкция\nпроверь hub и отчитайся",
  },
  {
    number: 440, title: "Крон: суточный semantic sync", state: "open",
    body: "---\nstatus: Вейт\nschedule: 0 5 * * *\nautodone: true\n---\n## Инструкция\nкрути индекс",
  },
  {
    number: 114, title: "QA: ночной аудит кода uwuwu-agent", state: "open",
    body: "---\nstatus: Вейт\nschedule: кривая строка\n---\nтело QA",
  },
  { number: 555, title: "Обычная задача в Todo", state: "open", body: "---\nstatus: Todo\n---\nтело" },
  { number: 556, title: "Паркованная в Вейт", state: "open", body: "---\nstatus: Вейт\n---\nтело" },
]

const stubGh = `#!/usr/bin/env bun
import { readFileSync } from "node:fs"
const fixtures = JSON.parse(readFileSync(new URL("./seam-fixtures.json", import.meta.url), "utf8")) as { number: number; body: string }[]
const a = process.argv.slice(2).join(" ")
if (a.includes("issues?labels=")) {
  process.stdout.write(JSON.stringify(fixtures))
  process.exit(0)
}
// Pv2-канон статусов (мастер-склейка #1097/1087): projectStatuses бьёт graphql
// двумя запросами — canned отвечает обоим шейпам; статусы в клетках берутся
// из issue.state-проекции, гейты файр-решений читают fm-attrs как раньше
if (a.includes("graphql")) {
  process.stdout.write(JSON.stringify({
    data: { viewer: {
      projectsV2: { nodes: [{ number: 5, title: "Задачи uwuwu" }] },
      projectV2: { items: { pageInfo: { hasNextPage: false, endCursor: null }, nodes: [
        { content: { number: 137 }, fieldValueByName: { name: "Вейт" } },
        { content: { number: 440 }, fieldValueByName: { name: "Вейт" } },
        { content: { number: 114 }, fieldValueByName: { name: "Вейт" } },
        { content: { number: 555 }, fieldValueByName: { name: "Todo" } },
        { content: { number: 556 }, fieldValueByName: { name: "Вейт" } },
      ] } },
    } },
  }))
  process.exit(0)
}
const m = /\\/issues\\/(\\d+)$/.exec(a)
if (m !== null) {
  const hit = fixtures.find((i) => i.number === Number(m[1]))
  if (hit === undefined) {
    process.stderr.write(\`stub-gh: нет фикстуры issue \${m[1]}\`)
    process.exit(1)
  }
  process.stdout.write(JSON.stringify(hit))
  process.exit(0)
}
process.stderr.write(\`stub-gh: неожиданный вызов: \${a}\`)
process.exit(1)
`

let tmpDir: string | null = null
let log = ""

const child = await (async (): Promise<{ code: number | null; stderr: string; timedOut: boolean }> => {
  tmpDir = mkdtempSync(join(tmpdir(), "uwuwu-cron-seam-"))
  const stubDir = join(tmpDir, "bin")
  const stateDir = join(tmpDir, "state")
  mkdirSync(stubDir, { recursive: true })
  mkdirSync(stateDir, { recursive: true })
  writeFileSync(join(stubDir, "gh"), stubGh)
  chmodSync(join(stubDir, "gh"), 0o755)
  // фикстуры рядом со stub: stub резолвит их от import.meta.url (bin/)
  writeFileSync(join(stubDir, "seam-fixtures.json"), JSON.stringify(FIXTURES))
  const ready = join(tmpDir, "ready")
  const logPath = join(tmpDir, "cron.log")
  const proc = Bun.spawn(["bun", CHILD, ready], {
    cwd: join(import.meta.dir, ".."),
    env: {
      ...process.env,
      UWUWU_CRON_DRY_RUN: "1",
      UWUWU_CRON_STATE_DIR: stateDir,
      UWUWU_CRON_LOG: logPath,
      UWUWU_HUB_REPO: "stub/uwuwu-hub",
      PATH: `${stubDir}:${process.env.PATH ?? ""}`,
    },
    stdout: "pipe",
    stderr: "pipe",
  })
  let timedOut = false
  const timer = setTimeout(() => {
    timedOut = true
    proc.kill()
  }, 30_000)
  const [exit, out, err] = await Promise.all([proc.exited, new Response(proc.stdout).text(), new Response(proc.stderr).text()])
  clearTimeout(timer)
  if (exit !== 0 || timedOut) console.error(`child stdout:\n${out}\nchild stderr:\n${err}`)
  log = existsSync(logPath) ? readFileSync(logPath, "utf8") : ""
  return { code: exit, stderr: err, timedOut }
})()

afterAll(() => {
  if (tmpDir !== null) rmSync(tmpDir, { recursive: true, force: true })
})

describe("tickBoard против stub-gh: шов доезжает до решений крона", () => {
  test("ребёнок отъехал чисто (stub-gh покрывает все вызовы тика)", async () => {
    const r = await child
    expect(r.timedOut).toBe(false)
    expect(r.code).toBe(0)
  })

  test("главный путь: разовый due-мастер дошёл до файр-решения (не скип по bodyDoc/attrs)", () => {
    // терминальная точка файр-пути в DRY_RUN: за ней только реальные записи
    expect(log).toContain("DRY_RUN: мастер «Lead: ночной аудит uwuwu» — ран-карточка и сессия не созданы (промпт ")
    // гейт прошёл: строка решена как файр, а не «строка снята»/«статус не Вейт»
    expect(log).not.toContain("строка снята")
    expect(log).not.toContain("статус не Вейт")
  })

  test("reconcile: cron-мастер с prevFire получил клеточный патч (linkedDocId+attrs+dateMs-гейт дошли)", () => {
    expect(log).toContain("мастер «Крон: суточный semantic sync»: клетки доски не сходятся (--planned ")
    expect(log).toContain("DRY_RUN: запись клеток пропущена")
  })

  test("отказ: кривое расписание прочитано из attrs и озвучено (не тихий скип null-семантики)", () => {
    expect(log).toContain("мастер «QA: ночной аудит кода uwuwu-agent»: кривое расписание «кривая строка»")
  })

  test("тихие скипы не озвучиваются: не-Вейт и Вейт без schedule молчат", () => {
    expect(log).not.toContain("Обычная задача в Todo")
    expect(log).not.toContain("Паркованная в Вейт")
  })

  test("файр-решение ровно одно: reconcile-мастер не уходит в файр на несовпадающей минуте", () => {
    expect(log.split("ран-карточка и сессия не созданы").length - 1).toBe(1)
  })
})
