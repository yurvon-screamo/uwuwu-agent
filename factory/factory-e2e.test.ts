// factory-e2e.test.ts — приёмочный e2e гварда спавна на живом hub (инцидент
// 10.11: burst-алерты спамили комменты задач). Гоняет ONCE-тики factory.ts
// в песочнице (UWUWU_FACTORY_STATE_DIR → свой state+лог, DRY_RUN=1 → все
// мутации досок и спавны невозможны) и проверяет наблюдаемое поведение:
// вход/выход эпизода в логе демона, ноль доставок алерта в задачи,
// per-project флаги (сосед не сносит чужой эпизод), heartbeat repeat,
// миграция легаси-state. Живой hub нужен только на чтение — та же культура
// гварда, что cli/tests/*_e2e.rs (UWUWU_E2E_GITHUB=1; без переменной скип).
// Тест читает реальную доску «Задачи uwuwu»: сценам burst нужен untracked
// больше порога — при тихой доске честно падает (сценарий не воспроизводим).
import { afterAll, describe, expect, test } from "bun:test"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const E2E = process.env.UWUWU_E2E_GITHUB === "1"
/** Живые ONCE-тики (~10с каждый: bun-старт + чтение досок) — дефолтный
 *  5с-таймаут bun test не годится. */
const E2E_TIMEOUT_MS = 300_000
const FACTORY_TS = join(import.meta.dir, "factory.ts")
/** Маркер текста burst-алерта: комменты задач с ним — доставка гварда. */
const ALERT_MARKER = "Спавн задержан"
/** Репо задач uwuwu (hub): адресат commentTask, приёмка «комменты чисты». */
const HUB_TASKS_REPO = "yurvon-screamo/uwuwu-hub"
/** Сосед из инцидента 10.11: тихий проект, чей allow-вердикт сносил общий
 *  флаг старого кода. В конфиге сцен — рядом с uwuwu. */
const NEIGHBOR = "true-washing"

interface HoldEpisode {
  since: string
  signaled: string
}
interface TickResult {
  log: string
  state: { holdEpisodes?: Record<string, HoldEpisode> }
}

const stateDirOf = (root: string): string => join(root, "state")
const logPathOf = (root: string): string => join(stateDirOf(root), "uwuwu-factory.log")
const statePathOf = (root: string): string => join(stateDirOf(root), "state.json")

/** Один ONCE-тик factory.ts в песочнице: свой state+лог, DRY_RUN, конфиг
 *  проектов uwuwu+сосед. Оверрайды env — поверх безопасного базиса. */
const runTick = async (
  root: string,
  overrides: Record<string, string>,
): Promise<TickResult> => {
  const proc = Bun.spawn({
    cmd: ["bun", FACTORY_TS],
    cwd: import.meta.dir,
    stdout: "pipe",
    stderr: "pipe",
    env: {
      ...process.env,
      UWUWU_FACTORY_STATE_DIR: stateDirOf(root),
      UWUWU_FACTORY_ONCE: "1",
      UWUWU_FACTORY_DRY_RUN: "1",
      UWUWU_FACTORY_POLL_SEC: "180",
      ...overrides,
    },
  })
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  const code = await proc.exited
  if (code !== 0) {
    throw new Error(`тик factory.ts упал (${code}): ${(err.trim() || out.trim()).slice(0, 400)}`)
  }
  const log = existsSync(logPathOf(root)) ? readFileSync(logPathOf(root), "utf-8") : ""
  const state = existsSync(statePathOf(root))
    ? (JSON.parse(readFileSync(statePathOf(root), "utf-8")) as TickResult["state"])
    : {}
  return { log, state }
}

/** Приёмка на живом hub: с момента since не появилось ни одного коммента
 *  с маркером алерта гварда (DRY_RUN-тик не доставляет, живой — не должен). */
const assertHubNoGuardComments = async (sinceIso: string): Promise<void> => {
  const proc = Bun.spawn({
    cmd: ["gh", "api", `repos/${HUB_TASKS_REPO}/issues/comments?since=${sinceIso}`],
    stdout: "pipe",
    stderr: "pipe",
  })
  const out = await new Response(proc.stdout).text()
  const code = await proc.exited
  if (code !== 0) throw new Error(`gh api не прошёл (${code}): ${out.slice(0, 300)}`)
  const hits = (JSON.parse(out) as Array<{ body: string }>).filter((c) =>
    c.body.includes(ALERT_MARKER),
  )
  expect(hits).toEqual([])
}

const projectsJson = JSON.stringify({
  uwuwu: { board: "Задачи uwuwu", repo: "yurvon-screamo/uwuwu-agent", local: "~/uwuwu-agent" },
  [NEIGHBOR]: { board: `Задачи ${NEIGHBOR}`, repo: `~/${NEIGHBOR}`, local: "~/true-wash" },
})
/** Burst-класс: порог untracked заведомо пробит, сироты в допуске. */
const BURST_ENV = {
  UWUWU_FACTORY_PROJECTS_JSON: projectsJson,
  UWUWU_FACTORY_SPAWN_BURST_MAX: "1",
  UWUWU_FACTORY_SPAWN_LOSS_MAX: "100",
}
/** Допуск всего (эксплуатация владельца 10.11 12:04: burst=9999). */
const ALLOW_ENV = {
  UWUWU_FACTORY_PROJECTS_JSON: projectsJson,
  UWUWU_FACTORY_SPAWN_BURST_MAX: "9999",
  UWUWU_FACTORY_SPAWN_LOSS_MAX: "100",
}

describe.skipIf(!E2E)("гвард спавна: живой цикл эпизода (инцидент 10.11)", () => {
  const roots: string[] = []
  const scratch = (): string => {
    const root = mkdtempSync(join(tmpdir(), "factory-guard-e2e-"))
    mkdirSync(stateDirOf(root), { recursive: true }) // для подкладывания state в сценах
    roots.push(root)
    return root
  }

  test("burst first — вход в эпизод в логе, ключ в state, ноль доставок в задачи", async () => {
    const root = scratch()
    const startedAt = new Date().toISOString()
    const tick = await runTick(root, BURST_ENV)

    // вход в эпизод: одна агрегированная строка лога с целями задержки
    const entries = tick.log.split("\n").filter((l) => l.includes("вход в burst-эпизод"))
    expect(entries).toHaveLength(1)
    expect(entries[0]).toContain("Цели задержки:")
    // эпизод персистится per-(project, reason); чужих ключей нет
    const episode = tick.state.holdEpisodes?.["uwuwu/burst"]
    expect(typeof episode?.since).toBe("string")
    expect(typeof episode?.signaled).toBe("string")
    expect(Object.keys(tick.state.holdEpisodes ?? {})).toEqual(["uwuwu/burst"])
    // главный инвариант бага: ни одной доставки алерта в задачи на живом hub
    await assertHubNoGuardComments(startedAt)
  }, E2E_TIMEOUT_MS)

  test("тик 2 — сосед allow не сносит эпизод, повторного входа нет", async () => {
    const root = scratch()
    const first = await runTick(root, BURST_ENV)
    const since = first.state.holdEpisodes?.["uwuwu/burst"]?.since
    expect(typeof since).toBe("string")

    // лог appендится в один файл: для второго тика берём только его хвост
    const second = await runTick(root, BURST_ENV)
    const secondLines = second.log.slice(first.log.length).split("\n")

    // корень спама 10.11: старый код сносил флаг allow-вердиктом соседа —
    // второй «вход» (alert=first каждый тик). Теперь в тике 2 ни входа,
    // ни сноса: эпизод продолжается молча (per-tick сводка — «задержано»).
    expect(secondLines.filter((l) => l.includes("вход в burst-эпизод"))).toEqual([])
    expect(secondLines.filter((l) => l.includes("сигнатура ушла"))).toEqual([])
    expect(secondLines.some((l) => l.includes("задержано гардом="))).toBeTrue()
    // флаг жив и не пересоздан: since — момент входа первого тика
    expect(second.state.holdEpisodes?.["uwuwu/burst"]?.since).toBe(since)
  }, E2E_TIMEOUT_MS)

  test("allow после порога 9999 — строка выхода с длительностью, ключ снят", async () => {
    const root = scratch()
    await runTick(root, BURST_ENV)
    const allow = await runTick(root, ALLOW_ENV)

    const ends = allow.log.split("\n").filter((l) => l.includes("эпизод HOLD (burst) завершён"))
    expect(ends).toHaveLength(1)
    expect(ends[0]).toMatch(/длительность \d+ мин/)
    expect(Object.keys(allow.state.holdEpisodes ?? {})).toEqual([])
  }, E2E_TIMEOUT_MS)

  test("легаси state (строка-флаг + мусор в holdEpisodes) — мигрирует без падения", async () => {
    const root = scratch()
    writeFileSync(
      statePathOf(root),
      JSON.stringify({
        tasks: {},
        holdSignaled: "2026-11-10T10:12:39.052Z",
        holdEpisodes: {
          "uwuwu/garbage": { since: 1, signaled: null },
          "мусор-без-слэша": { since: "a", signaled: "b" },
        },
      }),
    )
    const tick = await runTick(root, BURST_ENV)

    // легаси-флаг отброшен, эпизод самозаживился одним свежим входом
    const entries = tick.log.split("\n").filter((l) => l.includes("вход в burst-эпизод"))
    expect(entries).toHaveLength(1)
    expect(Object.keys(tick.state.holdEpisodes ?? {})).toEqual(["uwuwu/burst"])
    expect(typeof tick.state.holdEpisodes?.["uwuwu/burst"]?.since).toBe("string")
  }, E2E_TIMEOUT_MS)

  test("burst repeat (signaled за окном) — heartbeat в логе, since не двигается", async () => {
    const root = scratch()
    const oldIso = new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString()
    writeFileSync(
      statePathOf(root),
      JSON.stringify({
        tasks: {},
        holdEpisodes: { "uwuwu/burst": { since: oldIso, signaled: oldIso } },
      }),
    )
    const tick = await runTick(root, BURST_ENV)

    const beats = tick.log.split("\n").filter((l) => l.includes("burst-эпизод продолжается"))
    expect(beats).toHaveLength(1)
    // heartbeat сдвигает окно повтора, но не начало эпизода (длительность)
    expect(tick.state.holdEpisodes?.["uwuwu/burst"]?.since).toBe(oldIso)
    expect(tick.state.holdEpisodes?.["uwuwu/burst"]?.signaled).not.toBe(oldIso)
  }, E2E_TIMEOUT_MS)

  afterAll(() => {
    for (const root of roots) rmSync(root, { recursive: true, force: true })
  })
})
