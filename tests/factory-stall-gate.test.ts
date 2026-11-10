// tests/factory-stall-gate.test.ts — сталл-ветка и bodyEdit-гейт в тике
// фабрики (задача 20261109183658-kh3e7qe, PR #198): интеграционная граница
// «тик ↔ ядро/REST/gh». Главные пути фикса ложной будки дев-сессии:
//   • Review-гейт правки тела (отчёт тестировщика на Review-строке не будит
//     дева, базлайн обновляется — проглот осознанный);
//   • часы тишины от последнего сигнала сессии (свежий wake сбрасывает застой,
//     «wake + kick» одним тиком исключён; умершая будка кикается от честной
//     точки, gh-проба в окне расхождения не гоняется);
//   • merged-проба с дефером unknown и пределом деградации + фоллбек по ветке.
// Фабрика гоняется в спавн-ребёнке с HOME от старта процесса (инцидент 06.11);
// gh — фейк в песочнице PATH (GH_FAKE), журнал вызовов — GH_LOG.
import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { tmpdir } from "node:os"

const CHILD = join(import.meta.dir, "factory-stall-gate-child.ts")
const ROW_ID = "20261109000000-row1"
const PR42 = "https://github.com/r/x/pull/42"
const PR99 = "https://github.com/r/x/pull/99"
const DEV_SESSION = "ses_dev1"

interface TickObs {
  prompts: number
  statusUpdates: string[]
  comments: string[]
}

interface TickReport {
  scenario: string
  perTick: TickObs[]
  ghCalls: string[] | null
  state: {
    mergedPrUrl: string | null
    prProbeUnknownStreak: number | null
    stallSignaled: boolean | null
    bodyHash: string | null
    bodyUpdated: string | null
    lastStatus: string | null
  }
  boardStatus: string
  log: string[]
}

describe("tickProject: сталл-ветка и bodyEdit-гейт (ложная будка дев-сессии)", () => {
  let home: string
  let ghLog: string

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "factory-stall-gate-"))
    mkdirSync(join(home, ".local/state/uwuwu-factory"), { recursive: true })
    mkdirSync(join(home, ".local/state/opencode"), { recursive: true })
    mkdirSync(join(home, "bin"), { recursive: true })
    writeFileSync(
      join(home, ".local/state/opencode/service.json"),
      JSON.stringify({ url: "http://127.0.0.1:9", password: "pw" }),
    )
    // фейковый gh: merged/open/branch-merged/fail — поведение pr list/pr view;
    // журнал вызовов в GH_LOG (ленивость пробы и путь по клетке/ветке)
    const fakeGh = `#!/usr/bin/env bash
IS_LIST=false; for a in "$@"; do [[ "$a" == "list" ]] && IS_LIST=true; done
[[ -n "\${GH_LOG:-}" ]] && echo "$*" >> "$GH_LOG"
case "\${GH_FAKE:-fail}" in
  merged) if $IS_LIST; then echo "${PR42}"; else echo "MERGED"; fi ;;
  open) if $IS_LIST; then echo ""; else echo "OPEN"; fi ;;
  branch-merged) if $IS_LIST; then echo "${PR99}"; else echo "OPEN"; fi ;;
  *) exit 1 ;;
esac
`
    writeFileSync(join(home, "bin", "gh"), fakeGh, { mode: 0o755 })
    ghLog = join(home, "gh-calls.log")
  })

  afterEach(() => {
    rmSync(home, { recursive: true, force: true })
  })

  function seatState(
    scn: "review_swallow" | "wake_fresh" | "wake_dead" | "merged_cell" | "merged_cell_url" | "unknown_defer" | "branch_fallback",
  ): void {
    const promptAgeMin = scn === "wake_fresh" ? 1 : scn === "review_swallow" ? null : 400
    const state = {
      tasks: {
        [ROW_ID]: {
          sessionID: DEV_SESSION, worktree: "/wt/x", branch: "work/task-x",
          bodyDoc: "20261109000000-doc1", title: "Задача", project: "t",
          lastCommentAt: 0, lastCommentId: "", createdAt: 1, attempts: 0,
          lastStatus: scn === "review_swallow" ? "Review" : "In Progress",
          bodyUpdated: "20261106120000", bodyHash: "stale-baseline-hash",
          ...(promptAgeMin !== null ? { lastDevPromptAt: Date.now() - promptAgeMin * 60_000 } : {}),
        },
      },
    }
    writeFileSync(join(home, ".local/state/uwuwu-factory/state.json"), JSON.stringify(state))
  }

  async function runTick(
    scenario: "review_swallow" | "wake_fresh" | "wake_dead" | "merged_cell" | "merged_cell_url" | "unknown_defer" | "branch_fallback",
  ): Promise<TickReport> {
    seatState(scenario)
    const proc = Bun.spawn(["bun", CHILD], {
      env: {
        ...process.env,
        HOME: home,
        UWUWU_SIYUAN_TOKEN: "test-token",
        UWUWU_FACTORY_STATE_DIR: join(home, ".local/state/uwuwu-factory"),
        STALL_GATE_SCENARIO: scenario,
        GH_FAKE: scenario === "merged_cell" || scenario === "merged_cell_url"
          ? "merged"
          : scenario === "wake_dead" ? "open" : scenario === "branch_fallback" ? "branch-merged" : "fail",
        GH_LOG: ghLog,
        PATH: `${join(home, "bin")}:${process.env.PATH ?? ""}`,
      },
      stdout: "pipe", stderr: "pipe",
    })
    const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()])
    const code = await proc.exited
    if (code !== 0) throw new Error(`child упал (${code}): ${err.slice(0, 400)}`)
    const line = out.trim().split("\n").find((l) => l.startsWith("{"))
    if (line === undefined) throw new Error(`child не отчитался: ${out.slice(0, 200)} / ${err.slice(0, 200)}`)
    return JSON.parse(line) as TickReport
  }

  test("review: контентная правка тела (отчёт тестировщика) — будки дева нет, базлайн обновлён", async () => {
    const v = await runTick("review_swallow")
    // три тика (правки на тиках 1 и 3, тик 2 — без правки): ни одной будки
    for (const t of v.perTick) expect(t.prompts).toBe(0)
    expect(v.perTick[0]?.comments).toHaveLength(0)
    // базлайн двигается на каждом проглоте: повторные тики не пересчитывают
    // и не шумят (гейт — swallow с updateBaseline)
    expect(v.state.bodyUpdated).toBe("20261110120002")
    expect(v.state.bodyHash).not.toBe("stale-baseline-hash")
    expect(v.state.bodyHash).not.toBeNull()
    // осознанный проглот логируется, доска не трогается
    expect(v.log.join("\n")).toContain("правка тела на Review")
    expect(v.boardStatus).toBe("Review")
  })

  test("инцидентная геометрия 09.11: свежий wake сбрасывает часы — кика нет, gh не гоняется", async () => {
    const v = await runTick("wake_fresh")
    // тишина от активности 507 мин, будильник 1 мин — детекция ждёт ответ
    for (const t of v.perTick) {
      expect(t.prompts).toBe(0)
      expect(t.statusUpdates).toHaveLength(0)
      expect(t.comments).toHaveLength(0)
    }
    // ленивая проба: stalled=false — gh не вызывается ни разу (pre-чек на stallClock)
    expect(v.ghCalls).toBeNull()
    expect(v.state.stallSignaled).toBeNull()
    expect(v.state.prProbeUnknownStreak).toBeNull()
    expect(v.boardStatus).toBe("In Progress")
    expect(v.log.join("\n")).toContain("часы от wake")
  })

  test("будка умерла, PR открыт: кик от честной точки отсчёта, анти-спам раз за эпизод", async () => {
    const v = await runTick("wake_dead")
    // тик 1: сигнал — статус на доске + коммент юзеру; тишина считается от
    // последнего промпта (400 мин), не от последней активности (500 мин)
    expect(v.perTick[0]?.statusUpdates).toEqual(["k-status=Needs attention"])
    expect(v.perTick[0]?.comments).toHaveLength(1)
    expect(v.perTick[0]?.comments[0]).toContain("тишина 400 мин")
    // тик 2: анти-спам — повторного сигнала нет
    expect(v.perTick[1]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[1]?.comments).toHaveLength(0)
    expect(v.state.stallSignaled).toBe(true)
    expect(v.state.lastStatus).toBe("Needs attention")
    expect(v.boardStatus).toBe("Needs attention")
    // merged-проба по PR-клетке (gh pr view по URL из клетки, не по ветке)
    expect(v.ghCalls?.some((c) => c.includes("pr view"))).toBe(true)
  })

  test("PR смержен (text-клетка, путь prProbeByCell): кэш mergedPrUrl, кика нет", async () => {
    const v = await runTick("merged_cell")
    expect(v.perTick[0]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[0]?.comments).toHaveLength(0)
    expect(v.state.mergedPrUrl).toBe(PR42)
    expect(v.state.stallSignaled).toBeNull()
    expect(v.boardStatus).toBe("In Progress")
    expect(v.log.join("\n")).toContain("PR смержен")
  })

  test("PR смержен (url-клетка живой доски — после фикса адаптера): кэш из клетки, кика нет", async () => {
    const v = await runTick("merged_cell_url")
    expect(v.perTick[0]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[0]?.comments).toHaveLength(0)
    // кэш несёт URL ИЗ КЛЕТКИ (gh pr view) — проба по клетке на живой доске
    expect(v.state.mergedPrUrl).toBe(PR42)
    expect(v.ghCalls?.some((c) => c.includes("pr view"))).toBe(true)
    expect(v.ghCalls?.some((c) => c.includes("pr list"))).toBe(false)
    expect(v.state.stallSignaled).toBeNull()
    expect(v.boardStatus).toBe("In Progress")
  })

  test("gh лежит: unknown до предела откладывает кик, на 3-м тике деградация и кик", async () => {
    const v = await runTick("unknown_defer")
    // тики 1–2: полоса unknown — кик отложен, строка не тронута
    expect(v.perTick[0]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[1]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[0]?.comments).toHaveLength(0)
    // тик 3: страховка ADR-044 — деградация к not-merged, кик идёт
    expect(v.perTick[2]?.statusUpdates).toEqual(["k-status=Needs attention"])
    expect(v.perTick[2]?.comments).toHaveLength(1)
    expect(v.state.prProbeUnknownStreak).toBe(3)
    expect(v.state.stallSignaled).toBe(true)
    const joined = v.log.join("\n")
    expect(joined).toContain("кик отложен на тик")
    expect(joined).toContain("деградировал к not-merged")
    expect(v.boardStatus).toBe("Needs attention")
  })

  test("PR-клетка физически пуста (откат клетки): merged-PR найден фоллбеком по ветке", async () => {
    const v = await runTick("branch_fallback")
    expect(v.perTick[0]?.statusUpdates).toHaveLength(0)
    expect(v.perTick[0]?.comments).toHaveLength(0)
    // кэш несёт URL, найденный gh pr list --head (не клетку — она пуста)
    expect(v.state.mergedPrUrl).toBe(PR99)
    expect(v.ghCalls?.some((c) => c.includes("pr list"))).toBe(true)
    expect(v.boardStatus).toBe("In Progress")
    expect(v.log.join("\n")).toContain("PR смержен")
  })
})
