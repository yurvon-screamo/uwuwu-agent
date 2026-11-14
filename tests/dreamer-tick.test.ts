// tests/dreamer-tick.test.ts — интеграция тика uwuwu-dreamer: очередь сигналов
// фабрики → постановка задачного прогона → сбор пакета (BFS) → спавн
// дример-сессии. Демон гоняется в под-подпроцессе (HOME-песочница, фейк
// opencode REST) — state-машина чистых функций (task-run.test.ts) тут живёт
// в реальном тике с REST/FS-обвязкой.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const CHILD = join(import.meta.dir, "dreamer-tick-child.ts")
const ROW_ID = "20261109000000-tcktest1"
const DIRECTORY = "/home/x/uwuwu-agent"

interface TickVerdict {
  exitCode: number
  daemonLog: string
  daemonStderr: string
  sessions: Record<string, {
    status: string; dreamerSes: string | null; attempts: number
    lastProcessedUpdate: number; wokenAt: number
  }> | null
  taskRun: {
    status: string; dreamerSes: string | null; attempts: number
    lastProcessedUpdate: number; rootSessions: string[]
  } | null
  activeSessions: Record<string, string> | null
  queue: string[]
  sessionCreates: Array<{ body: { agent?: string; parentID?: string; location?: { directory?: string } } }>
  prompts: Array<{ path?: string; body: { text?: string } }>
  sessionDeletes: unknown[]
  strictPathPin?: boolean
}

describe("тик дримера: задачный контур (Done → пакетный прогон)", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "dreamer-tick-"))
    mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
  })

  afterEach(() => {
    rmSync(home, { recursive: true, force: true })
  })

  async function runTick(scenario: "task-spawn" | "empty-batch"): Promise<TickVerdict> {
    const proc = Bun.spawn(["bun", CHILD], {
      env: { ...process.env, HOME: home, SCENARIO: scenario },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${code}): ${(err.trim() || out.trim()).slice(0, 300)}`)
    return JSON.parse(out.trim()) as TickVerdict
  }

  test("done-сигнал → задачный прогон спавнит дримера: трек running, пакет с BFS-детьми, no DELETE", async () => {
    const verdict = await runTick("task-spawn")
    expect(verdict.exitCode).toBe(0)
    // регресс-пин класса PR #207: мок отвергает двойной /api-префикс (не-ок ответ)
    expect(verdict.strictPathPin).toBe(true)

    // очередь потреблена: start (мерж активного набора) и done (постановка)
    expect(verdict.queue).toEqual([])
    // активный набор жив до финала прогона — посессионный контур скипает сессию
    expect(verdict.activeSessions).toEqual({ ses_dev1: ROW_ID })

    // трек задачного прогона: pending → running, дример-сессия привязана
    expect(verdict.taskRun).not.toBeNull()
    expect(verdict.taskRun?.status).toBe("running")
    expect(verdict.taskRun?.dreamerSes).toBe("ses_dreamfake01")
    expect(verdict.taskRun?.attempts).toBe(0)

    // спавн без parentID, в мастер-чекаут проекта, агент dreamer
    expect(verdict.sessionCreates).toHaveLength(1)
    const create = verdict.sessionCreates[0]?.body
    expect(create?.agent).toBe("dreamer")
    expect(create?.location?.directory).toBe(DIRECTORY)
    expect(create?.parentID).toBeUndefined()

    // один промпт; пакет — вся история корней + BFS-ребёнок (ses_plan1)
    expect(verdict.prompts).toHaveLength(1)
    const prompt = verdict.prompts[0]?.body?.text ?? ""
    expect(prompt).toContain(ROW_ID)
    expect(prompt).toContain("ses_dev1")
    expect(prompt).toContain("ses_plan1")
    expect(prompt).toContain("ses_rev1")
    expect(prompt).toContain("ses_test1")

    // никаких DELETE — сессии задачи и димера остаются в БД (решение владельца)
    expect(verdict.sessionDeletes).toHaveLength(0)
  })

  test("пустой пакет (корни не существуют) → честный done без прогона, активный набор снят", async () => {
    const verdict = await runTick("empty-batch")
    expect(verdict.exitCode).toBe(0)

    expect(verdict.queue).toEqual([])
    expect(verdict.taskRun?.status).toBe("done")
    expect(verdict.taskRun?.dreamerSes).toBeNull()
    // сессии задачи сняты с активного набора — fallback посессионного контура
    expect(verdict.activeSessions).toEqual({})
    // нечего анализировать — спавнов и промптов нет
    expect(verdict.sessionCreates).toHaveLength(0)
    expect(verdict.prompts).toHaveLength(0)
    expect(verdict.sessionDeletes).toHaveLength(0)
  })
})

describe("тик дримера: посессионный контур (баг 1077 — detached-спавн без parentID)", () => {
  let home: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "dreamer-tick-"))
    mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
  })

  afterEach(() => {
    rmSync(home, { recursive: true, force: true })
  })

  async function runTick(scenario: "session-new" | "session-legacy-migrate" | "session-dead-dir"): Promise<TickVerdict> {
    const proc = Bun.spawn(["bun", CHILD], {
      env: { ...process.env, HOME: home, SCENARIO: scenario },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child (${code}): ${(err.trim() || out.trim()).slice(0, 300)}`)
    return JSON.parse(out.trim()) as TickVerdict
  }

  test("новый кандидат → detached-спавн: POST /session без parentID, location = живой каталог исходника", async () => {
    const verdict = await runTick("session-new")
    expect(verdict.exitCode).toBe(0)

    // спавн ровно один, без parentID, с явным location каталога исходной сессии
    expect(verdict.sessionCreates).toHaveLength(1)
    const create = verdict.sessionCreates[0]?.body
    expect(create?.agent).toBe("dreamer")
    expect(create?.parentID).toBeUndefined()
    expect(create?.location?.directory).toBe(join(home, "proj"))

    // один промпт — новой дример-сессии, текст про исходник
    expect(verdict.prompts).toHaveLength(1)
    expect(verdict.prompts[0]?.path).toBe("/api/session/ses_dreamfake01/prompt")
    expect(verdict.prompts[0]?.body?.text ?? "").toContain("ses_orig1")

    // трек: running, dreamerSes привязан
    expect(verdict.sessions?.ses_orig1?.status).toBe("running")
    expect(verdict.sessions?.ses_orig1?.dreamerSes).toBe("ses_dreamfake01")

    // никаких DELETE — сессии остаются в БД
    expect(verdict.sessionDeletes).toHaveLength(0)
  })

  test("legacy parentID-child → миграция: старому child нет промпта, спавн новой detached-сессии", async () => {
    const verdict = await runTick("session-legacy-migrate")
    expect(verdict.exitCode).toBe(0)

    // лог называет миграцию
    expect(verdict.daemonLog).toContain("legacy parentID-child")

    // спавн detached: без parentID, location = каталог исходника
    expect(verdict.sessionCreates).toHaveLength(1)
    const create = verdict.sessionCreates[0]?.body
    expect(create?.agent).toBe("dreamer")
    expect(create?.parentID).toBeUndefined()
    expect(create?.location?.directory).toBe(join(home, "proj"))

    // промпты: только новой сессии (ses_dreamfake01); legacy-child
    // (ses_dreamlegacy) промпта не получил — заморожен
    expect(verdict.prompts).toHaveLength(1)
    expect(verdict.prompts[0]?.path).toBe("/api/session/ses_dreamfake01/prompt")
    const promptPaths = verdict.prompts.map((p) => p.path ?? "")
    expect(promptPaths.some((p) => p.includes("ses_dreamlegacy"))).toBe(false)

    // трек перепривязан на новую сессию; revived сбросил attempts в 0
    expect(verdict.sessions?.ses_orig1?.dreamerSes).toBe("ses_dreamfake01")
    expect(verdict.sessions?.ses_orig1?.attempts).toBe(0)
    expect(verdict.sessions?.ses_orig1?.status).toBe("running")

    // заморозка без DELETE — история legacy-child остаётся в БД
    expect(verdict.sessionDeletes).toHaveLength(0)
  })

  test("каталог исходника не существует → спавн без location (гвард живости каталога)", async () => {
    const verdict = await runTick("session-dead-dir")
    expect(verdict.exitCode).toBe(0)

    expect(verdict.sessionCreates).toHaveLength(1)
    const create = verdict.sessionCreates[0]?.body
    expect(create?.agent).toBe("dreamer")
    expect(create?.parentID).toBeUndefined()
    expect(create?.location).toBeUndefined()

    // спавн состоялся, трек жив — гвард не блокирует прогон
    expect(verdict.sessions?.ses_orig1?.status).toBe("running")
    expect(verdict.sessions?.ses_orig1?.dreamerSes).toBe("ses_dreamfake01")
  })
})
