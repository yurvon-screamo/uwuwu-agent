// uwuwu-factory — будильник фабрики разработки + маршрутизатор ревью (hub).
// Вся логика пайпа — у агентов (глобальный AGENTS.md, раздел «Работа над
// задачей»). Демон делает четыре вещи:
//   1. Design на доске (Draft = черновик на валидации триажа, пустой =
//      легаси-черновик юзера — оба не трогаем) → сессия девелопера в
//      чекауте репо + spawn-промпт утряски (ворктри/ветку создаёт сам
//      агент); In Progress без записи state — fast-track владельца →
//      implement-промпт (сразу реализация, без Design/план-ревью);
//      перевод владельца в In Progress у трекенной задачи — будильник/
//      рестарт девелопера (return-to-dev), PR в конфликте с master →
//      конфликт-kickoff (разреши, запушь, верни в «Ревью результата»)
//   2. «Ревью плана» → спавн/будильник план-ревьювера (праймари
//      plan-reviewer, своя сессия на стадию, cwd = ворктри задачи);
//      «Ревью результата» → спавн/будильник ревьювера результата (праймари
//      code-quality-reviewer, своя сессия на стадию, cwd = ворктри
//      задачи); «Тестирование» → спавн/будильник тестировщика (праймари
//      tester, тоже 1 сессия на задачу); возврат статуса из ревью/тестирования
//      → wakeup девелопера; комменты юзера в агентских статусах — держателю
//      сессии
//   3. новые комменты/правки тела юзера → wakeup сессии (мёртвая → карантин,
//      коммент юзера → рестарт)
//   4. Done (кто бы ни поставил — мерж-крон агента или юзер) → чистка
//      state + термин живой ревью-сессии
//   5. In Progress: сессия жива, юзер и агент молчат дольше порога
//      (UWUWU_FACTORY_STALL_MIN, деф. 360 = 6 ч — трейд владельца 08.11:
//      глубокие фоновые прогоны легитимно молчат часами) → сигнал-коммент
//      + Needs attention раз за эпизод (потерянный/зависший ход: активный
//      ход рождает сообщения)
// Запуск: systemd --user (uwuwu-factory.service). Тик: 3 минуты.

import { appendFileSync, copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync, renameSync, existsSync, rmSync } from "node:fs"
import { homedir, hostname } from "node:os"
import { dirname, join, resolve } from "node:path"
import * as mcp from "../lib/platform"
import { parseCommentsMarkdown } from "../lib/platform"
import { PLAN_REVIEWER_AGENT_ID, REVIEWER_AGENT_ID, TESTER_AGENT_ID } from "../lib/agents"
import { acquireLock, makeDrain, resolveDaemonStateDir, daemonLogPath, emitNotice } from "../lib/single-instance"
import {
  newUserComments,
  untrackedTasks,
  isUserComment,
  kickoffModeOf,
  baseOf,
  promptForStart,
  prMergeableOf,
  mergeablePollStep,
  resolvePromptWorktree,
  type MergeablePollStep,
  wakeupPrompt,
  bodyEditPrompt,
  isValidTracked,
  reviewKindOf,
  reviewSpawnPrompt,
  reviewRerunPrompt,
  reviewCommentsPrompt,
  reviewReturnPrompt,
  reviewStallVerdict,
  sessionSlotOf,
  TESTING_STATUS,
  TEST_SESSION_CELL,
  testingSpawnPrompt,
  testingRerunPrompt,
  testingCommentsPrompt,
  conflictFixPrompt,
  routeTask,
  rowMissingAction,
  runBatch,
  parseProjects,
  formatPassSummary,
  stallMinutesFromEnv,
  lastActivityOf,
  stallVerdict,
  stallClock,
  bodyEditRoute,
  prProbeVerdict,
  PR_PROBE_UNKNOWN_MAX,
  type PrProbeResult,
  adoptBodyDoc,
  pathSafe,
  createBoardPathOnce,
  errText,
  USER_REVIEW_STATUS,
  AGENT_MARK,
  stripExportWrapper,
  alertTargetOf,
  spawnGuardVerdict,
  spawnLimitsFromEnv,
  untrackedRowDecision,
  activeTaskGuards,
  parseAgentHeadTs,
  freshForeignComments,
  legacyRebaselineNeeded,
  latestComment,
  bodyContentHash,
  bodyLastmodMs,
  wakeupDedup,
  PATH_CELL,
  DEV_SESSION_CELL,
  REVIEW_SESSION_CELL,
  type PassCounters,
  type ProjectCfg,
  type ReviewKind,
  type FactoryState,
  type SpawnInit,
  type StartKind,
  type SessionLiveness,
  type SpawnEnvContext,
  type DisplayLease,
  type SpawnGuardHold,
  type TaskComment,
  type TaskRef,
  type TrackedTask,
  activeTaskCountEx,
  cupBudget,
  queueOrder,
  peakWindowFromEnv,
  isPeakHours,
  type PeakWindow,
} from "./factory-lib"
import {
  prCarrierOf,
  githubProbeVerdict,
  giteaProbeVerdict,
  autodoneAction,
  type PrCarrier,
  type PrProbeState,
} from "./autodone"
import { checkNodePinning, failPinnedStart } from "../lib/node-pinning"
import { ensureEnv, cleanupEnv, sweepEnvs, sanitizeRowId, displayFilePath, readDisplayNumber, type EnvDeps } from "./env"
import { gcLegacyWorktrees, MERGE_GRACE_MS_DEFAULT } from "./worktree-gc"

// ——— конфиг проектов (только точки входа; пайпы живут в AGENTS.md репо) ———
// ProjectCfg и env-оверрайд (UWUWU_FACTORY_PROJECTS_JSON) — factory-lib;
// здесь только прод-дефолт
export type { ProjectCfg }

const DEFAULT_PROJECTS: Record<string, ProjectCfg> = {
  uwuwu: {
    board: "Задачи uwuwu",
    repo: "yurvon-screamo/uwuwu-agent",
    local: join(homedir(), "uwuwu-agent"),
  },
  origa: {
    board: "Задачи origa",
    repo: "yurvon-screamo/origa",
    local: join(homedir(), "origa"),
  },
  foilcap: {
    board: "Задачи foilcap",
    repo: "git.l4a.ru/ek/ai-generator",
    local: join(homedir(), "foilcap-ai-generator"),
  },
  "true-washing": {
    board: "Задачи true-washing",
    repo: "~/true-washing (detector, saas — репо в подпапках)",
    local: join(homedir(), "true-wash"),
    env: false, // cfg.local — не git-репо: env-контракт невыполним, агент сам делает ворктри
  },
}
// state: песочница — UWUWU_FACTORY_STATE_DIR двигает state+lock, в песочнице
// лог тоже переезжает (прод-лог не засоряется смоук-тиками). Дефолт-изоляция:
// не-master чекаут без env получает свой state в branches/<ветка> и прод не
// трогает (инцидент-класс 06.11 — прогон из ветки писал общий прод-state);
// master — прежний прод-путь.
const STATE_RESOLVED = resolveDaemonStateDir({
  daemon: "uwuwu-factory",
  envName: "UWUWU_FACTORY_STATE_DIR",
  envRaw: process.env.UWUWU_FACTORY_STATE_DIR,
  defaultDir: join(homedir(), ".local/state/uwuwu-factory"),
  repoRoot: resolve(import.meta.dir, ".."),
})
const STATE_DIR = STATE_RESOLVED.stateDir
const STATE_FILE = join(STATE_DIR, "state.json")
const LOG_PATH = daemonLogPath({
  stateDir: STATE_DIR, sandboxed: STATE_RESOLVED.sandboxed,
  fileName: "uwuwu-factory.log", homeDir: homedir(),
})
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")
const POLL_SEC = Number(process.env.UWUWU_FACTORY_POLL_SEC ?? 180)
/** порог тишины сессии: активный ход рождает сообщения на каждом шаге LLM —
 *  тишина дольше порога = потерянный/зависший ход. Парсер валидирует env
 *  (0/NaN/мусор → дефолт 30), эффективное значение — в стартовый лог. */
const STALL_MIN = stallMinutesFromEnv(process.env.UWUWU_FACTORY_STALL_MIN)
const STALL_MS = STALL_MIN * 60_000
/** гард каскада (инцидент 06.11): пустой/резко уменьшившийся state.json не
 *  должен превращаться в волну re-spawn. Сверка с доской до спавна —
 *  сироты-спавны (строка с сессионной ячейкой без трека) и рубеж burst;
 *  эффективные значения — в стартовый лог. */
const SPAWN_LIMITS = spawnLimitsFromEnv(
  process.env.UWUWU_FACTORY_SPAWN_LOSS_MAX,
  process.env.UWUWU_FACTORY_SPAWN_BURST_MAX,
)
/** разовый тик и выход — для живого смоук-теста */
const ONCE = process.env.UWUWU_FACTORY_ONCE === "1"
/** DRY_RUN: полный цикл решений по реальным доскам (чтение), все мутации
 *  (спавны, промпты, ячейки, комменты, термин сессий) заменяются логом.
 *  Требует песочницу state — гард в main(). */
const DRY_RUN = process.env.UWUWU_FACTORY_DRY_RUN === "1"
/** CUP (задача 20261112160316): кап одновременно активных (LLM-тратящих)
 *  задач глобально по всем проектам — GLM-подписка одна. Мусор env → 7. */
const CUP_RAW = Number(process.env.UWUWU_FACTORY_CUP ?? 7)
const CUP = CUP_RAW > 0 ? CUP_RAW : 7
/** Пик-окно GLM Coding Plan (см. peakWindowFromEnv): дефолт 06:00–10:00 UTC
 *  = 15:00–19:00 JST, "off" отключает заморозку. */
const PEAK: PeakWindow = peakWindowFromEnv(process.env.UWUWU_FACTORY_PEAK)
/** Таймаут ожидания зелёного CI автодона (pending-проба) — дальше Needs
 *  attention (не bounce: висящий CI не вина задачи). */
const AUTODONE_TIMEOUT_RAW = Number(process.env.UWUWU_FACTORY_AUTODONE_TIMEOUT_MIN ?? 60)
const AUTODONE_TIMEOUT_MS = AUTODONE_TIMEOUT_RAW > 0 ? AUTODONE_TIMEOUT_RAW * 60_000 : 60 * 60_000
const MODEL = { providerID: "zai-coding-plan", id: "glm-5.3-flash" } as const
/** ревьювер результата («Ревью результата»): праймари-агент, качество важнее цены
 *  будильника. «Ревью плана» ведёт PLAN_REVIEWER ниже. Модель дублирует
 *  agents/code-quality-reviewer.md (frontmatter) — править синхронно: модель сессии
 *  спавна главнее для фабричных сессий. */
const REVIEWER = { agent: REVIEWER_AGENT_ID, model: { providerID: "zai-coding-plan", id: "glm-5.3" } } as const
/** план-ревьювер стадии «Ревью плана»: парафриз, zoom-out (YAGNI/реюз/глубина
 *  постановки), implementation readiness. Модель дублирует agents/plan-reviewer.md
 *  (frontmatter) — править синхронно: модель сессии спавна главнее для фабричных сессий. */
const PLAN_REVIEWER = { agent: PLAN_REVIEWER_AGENT_ID, model: { providerID: "zai-coding-plan", id: "glm-5.3" } } as const
/** тестировщик стадии «Тестирование»: праймари-агент, качество фичи важнее
 *  цены будильника; модель flash в max-вариации — единственная мультимодальная
 *  (визуальная проверка вёрстки/скринов). Модель дублирует agents/tester.md
 *  (frontmatter) — править синхронно: модель сессии спавна главнее для фабричных
 *  сессий. */
const TESTER = {
  agent: TESTER_AGENT_ID,
  model: { providerID: "zai-coding-plan", id: "glm-5.3-flash", variant: "max" },
} as const
const SPAWN_TIMEOUT_MS = 60_000
const REST_TIMEOUT_MS = 15_000
const MAX_ATTEMPTS = 3
/** параллельность Design-волны: create+prompt+liveness ≈ 5 сетевых шагов
 *  на задачу — 4 дорожки держат волну из ~10 задач в пределах полутика */
const SPAWN_WAVE_CONCURRENCY = 4
/** бот-аккаунт афины: его комменты — не сигналы юзера */
const BOT_USER_ID = process.env.UWUWU_BOT_USER_ID ?? "df415887-98cc-4147-b5df-b6c71d8afeae"
/** окно дедупа повторной доставки будильника (дефект №5.2: 7 будок за 9 минут) */
const WAKEUP_DEDUP_MS = 10 * 60_000
/** корень окружений задач: <root>/<row-id>/{repo,scratch,logs} (на /home,
 *  не tmpfs — урок SIGSEGV 03.11). Песочница переопределяет вместе с state. */
const ENV_ROOT = process.env.UWUWU_FACTORY_ENV_ROOT ?? join(homedir(), "agents-env")
const ENV_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000
/** периодичность GC легаси-ворктри в тиках: при poll 180s ≈ раз в час.
 *  Старт-проход отдельный — рестарт демона не обязан ждать час. */
const GC_EVERY_TICKS = 20

/** Исполнитель git-операций окружения: прод — фабричный sh() с таймаутом. */
const envDeps = (): EnvDeps => ({ run: (cmd, cwd) => sh(cmd, { cwd }), log })

const log = (m: string): void => {
  try { appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`) } catch {}
}

function rotateLog(): void {
  try {
    if (!existsSync(LOG_PATH)) return
    if (statSync(LOG_PATH).size > 5 * 1024 * 1024) {
      rmSync(`${LOG_PATH}.old`, { force: true })
      renameSync(LOG_PATH, `${LOG_PATH}.old`)
    }
  } catch {}
}

// ——— одиночный инстанс ———

// onStop лока взводит флаг: проход доезжает до границы, новую работу не спавним
const drain = makeDrain()

// ——— состояние (валидация + атомарная запись) ———

function loadState(): FactoryState {
  try {
    const raw: unknown = JSON.parse(readFileSync(STATE_FILE, "utf-8"))
    const tasks = (raw as { tasks?: unknown }).tasks
    const out: FactoryState = { tasks: {} }
    // флаг эпизода гарда — top-level (не задачи): переносится между тиками,
    // снимается allow-вердиктом (см. tickProject)
    const holdSignaled = (raw as { holdSignaled?: unknown }).holdSignaled
    if (typeof holdSignaled === "string") out.holdSignaled = holdSignaled
    if (tasks !== null && typeof tasks === "object") {
      for (const [k, v] of Object.entries(tasks as Record<string, unknown>)) {
        if (isValidTracked(v)) {
          out.tasks[k] = v
        } else {
          log(`state: запись ${k.slice(0, 10)} битая — пропущена`)
        }
      }
    }
    return out
  } catch {
    return { tasks: {} }
  }
}

function saveState(state: FactoryState): void {
  mkdirSync(STATE_DIR, { recursive: true })
  if (existsSync(STATE_FILE)) {
    try { copyFileSync(STATE_FILE, `${STATE_FILE}.bak`) } catch {}
  }
  const tmp = `${STATE_FILE}.tmp`
  writeFileSync(tmp, JSON.stringify(state, null, 2))
  renameSync(tmp, STATE_FILE)
}

// ——— процессы (git/gh/uwuwu-cli) с таймаутом ———

async function sh(cmd: string[], opts: { cwd?: string } = {}): Promise<string> {
  const proc = Bun.spawn({ cmd, cwd: opts.cwd, stdout: "pipe", stderr: "pipe" })
  const killer = setTimeout(() => proc.kill(), SPAWN_TIMEOUT_MS)
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  const code = await proc.exited
  clearTimeout(killer)
  if (code !== 0) throw new Error(`${cmd[0]} (${code}): ${(err.trim() || out.trim()).slice(0, 200)}`)
  return out.trim()
}

// ——— доска задач ———

interface BoardIds { docId: string; dbId: string }

/** Контракт колонок автодона на досках задач (задача 20261112160316):
 *  «Приоритет» select + «Autodone» checkbox — постановка сама требует
 *  колонки, это явное поручение на доску-мутацию. Идемпотентно, на старте
 *  демона по всем проектным доскам; сбой одного проекта не глушит старт. */
const AUTODONE_COLUMNS = [
  { name: "Приоритет", type: "select" },
  { name: "Autodone", type: "checkbox" },
]

async function ensureBoardColumns(projects: Record<string, ProjectCfg>): Promise<void> {
  if (DRY_RUN) {
    log("DRY_RUN: ensure колонок автодона пропущен (мутация доски)")
    return
  }
  for (const [name, cfg] of Object.entries(projects)) {
    try {
      const board = await findBoard(cfg.board)
      const r = await mcp.tool<{ created?: string[] }>("ensure_database_columns", {
        docId: board.docId, databaseBlockId: board.dbId, columns: AUTODONE_COLUMNS,
      })
      if ((r.created ?? []).length > 0) {
        log(`колонки автодона на «${cfg.board}» минчены: ${(r.created ?? []).join(", ")}`)
      }
    } catch (e) {
      log(`колонки автодона [${name}]: ${errText(e)} — задачи проекта без колонок читаются пустыми, retry на следующем старте`)
    }
  }
}

async function findBoard(title: string): Promise<BoardIds> {
  const r = await mcp.tool<{ matches?: { id: string; inTrash?: boolean }[] }>("find_doc_by_title", { title, caseInsensitive: true, limit: 5 })
  let docId = r.matches?.find(m => m.inTrash !== true)?.id
  if (docId === undefined) {
    // индекс тайтлов отстаёт от свежих/переименованных доков — фоллбек
    // через организ-дерево: «Задачи X» → папка X → дети по точному тайтлу
    docId = (await boardViaOrganize(title)) ?? ""
  }
  if (docId === "") throw new Error(`доска «${title}» не найдена`)
  const doc = await mcp.tool<{ blocks?: { id: string; flavour: string }[] }>("read_doc", { docId })
  const dbId = doc.blocks?.find((b) => b.flavour === "platform:database")?.id
  if (dbId === undefined) throw new Error(`в «${title}» нет database-блока`)
  return { docId, dbId }
}

/** Доска без поискового индекса: «Задачи <proj>» → папка <proj> → доки. */
async function boardViaOrganize(title: string): Promise<string | null> {
  const proj = title.startsWith("Задачи ") ? title.slice("Задачи ".length) : null
  if (proj === null) return null
  try {
    const r = await mcp.tool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>(
      "list_organize_nodes", {})
    const nodes = r.nodes ?? []
    const folders = nodes.filter(n => n.type === "folder" && n.data === proj)
    for (const f of folders) {
      const docIds = nodes
        .filter(n => n.type === "doc" && n.parentId === f.id)
        .map(n => n.data)
        .filter((d): d is string => typeof d === "string")
      for (const docId of docIds) {
        const d = await mcp.tool<{ title?: string }>("get_doc", { docId })
        if (d.title === title) return docId
      }
    }
  } catch { /* организ недоступен — остаётся ошибка поиска */ }
  return null
}

export async function readTasks(
  board: BoardIds,
  known: ReadonlyMap<string, string> = new Map(),
): Promise<TaskRef[]> {
  const r = await mcp.tool<{
    rows?: {
      rowBlockId?: string
      linkedDocId?: string | null
      title?: string | null
      cells?: Record<string, { value?: string }>
    }[]
  }>("read_database_cells", { docId: board.docId, databaseBlockId: board.dbId })
  const raw = (r.rows ?? []).filter((row) => typeof row.rowBlockId === "string")

  // Фоллбек тела по заголовку для detached-строк (привязки нет) — паритет
  // CLI body_doc_by_title: саб-док доски по hpath «<доска>/<тайтл>».
  // FS-истина (getIDsByHPath) — без лага SQL-индекса. 0 → без тела,
  // 1 → тело, ≥2 → пусто (не угадываем) + агрегированный лог сирот.
  // get_doc доски — один раз на проход (ленивый мемоизер: сбой и SQL-лаг
  // тоже кэшируются — N detached-строк дают один вызов и одну строку лога;
  // без detached-строк доска не читается вовсе).
  const boardPathOnce = createBoardPathOnce(
    () => mcp.tool<{ box?: string; hpath?: string }>("get_doc", { docId: board.docId }),
    log,
  )
  const orphans: string[] = []
  const rows: TaskRef[] = []
  for (const row of raw) {
    // hub-эпоха (#1126): read_database_cells отдаёт тайтл в cells["Название"],
    // тело строки — сам issue (docId «issue:<номер>»); title/linkedDocId
    // верхнего уровня — легаси-формат прошлой платформы, фоллбек сохранён.
    const isHubRow = row.cells?.Название !== undefined
    const title = (row.title ?? row.cells?.Название?.value ?? "").trim()
    let bodyDoc = row.linkedDocId ?? (isHubRow ? `issue:${row.rowBlockId}` : "")
    if (bodyDoc === "") {
      // Трек уже знает тело (self-heal прошлых тиков): state выигрывает у
      // фоллбека без единого API-вызова — per-tick трафик не растёт с числом
      // detached-строк. Результат фоллбека для такого трека и сегодня
      // отбрасывался бы (adopt идемпотентен, комменты читаются из
      // tracked.bodyDoc). linkedDocId по-прежнему всегда выигрывает над state.
      const knownDoc = known.get(row.rowBlockId as string) ?? ""
      if (knownDoc !== "") bodyDoc = knownDoc
      else if (title !== "") {
        const bp = await boardPathOnce()
        if (bp.kind === "active") {
          try {
            const found = (await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", {
              notebook: bp.box, path: `${bp.hpath}/${pathSafe(title)}`,
            })).ids ?? []
            if (found.length === 1) bodyDoc = found[0] as string
            else if (found.length >= 2) orphans.push(`${String(row.rowBlockId).slice(0, 8)} → ${found.join(", ")}`)
          } catch (e) {
            // фоллбек — best effort: сбой ядра на одной detached-строке не должен
            // глушить тик (комменты/будильники здоровых привязанных строк)
            log(`задача ${String(row.rowBlockId).slice(0, 8)}: фоллбек тела по тайтлу не удался (${errText(e)}) — строка без тела`)
          }
        }
        // failed/disabled: деградация уже залогирована резолвером одной строкой
        // на проход — per-row спам не дублируем, строка остаётся без тела
      }
    }
    rows.push({
      rowId: row.rowBlockId as string,
      bodyDoc,
      title,
      status: row.cells?.Status?.value ?? "",
      type: (row.cells?.Тип?.value ?? "").trim(),
      priority: (row.cells?.Приоритет?.value ?? "").trim(),
      autodone: (row.cells?.Autodone?.value ?? "") === "true",
      pr: row.cells?.PR?.value ?? "",
      branch: row.cells?.Branch?.value ?? "",
      devSession: (row.cells?.[DEV_SESSION_CELL]?.value ?? "").trim(),
      reviewSession: (row.cells?.[REVIEW_SESSION_CELL]?.value ?? "").trim(),
      testSession: (row.cells?.[TEST_SESSION_CELL]?.value ?? "").trim(),
    })
  }
  if (orphans.length > 0) {
    log(`дубль-сироты: ${orphans.length} строк(и) с ≥2 доками одного тайтла — чисти руками в UI: ${orphans.join("; ")}`)
  }
  return rows
}

async function setCells(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<string[]> {
  if (DRY_RUN) {
    log(`DRY_RUN: setCells ${rowId.slice(0, 8)} ${JSON.stringify(cells)} — пропущен`)
    return []
  }
  const r = await mcp.tool<{ skipped?: string[] }>("update_database_row", {
    docId: board.docId, databaseBlockId: board.dbId, rowBlockId: rowId, cells,
  })
  return r.skipped ?? []
}

/** Best-effort запись служебных ячеек (Path/сессии/Branch): колонку владелец
 *  мог не завести — тихий no-op ядра или ошибка не ломают спавн. Возврат —
 *  имена колонок, ушедших в no-op: вызывающий решает, громко ли флаговать. */
async function setCellsSafe(board: BoardIds, rowId: string, cells: Record<string, string>): Promise<string[]> {
  try {
    return await setCells(board, rowId, cells)
  } catch (e) {
    log(`ячейки ${rowId.slice(0, 8)} (${Object.keys(cells).join(", ")}): ${errText(e)} — продолжаю без записи`)
    return []
  }
}

/** Комменты тела из экспорта + parseCommentsMarkdown (секции «## Комменты» и
 *  трейлинг-хвост — CLI-аппенд в конец дока). exportCache — per-tick кэш
 *  экспортов: тик читает тело один раз для комментов и хэша (дубль тяжёлого
 *  вызова ядра — Low-1 ревью раунда 3). Тред-ответы — префикс-якорь родителя
 *  для контекста ленты; сам ответ в тред — `task comment --reply` (uwuwu-cli). */
async function taskComments(bodyDoc: string, exportCache?: Map<string, string | null>): Promise<TaskComment[]> {
  if (bodyDoc === "") return []
  const md = await bodyExport(bodyDoc, exportCache)
  if (md === null) return []
  const parsed = parseCommentsMarkdown(md, bodyDoc)
  const out: TaskComment[] = []
  for (const c of parsed) {
    out.push({
      id: c.id,
      text: c.text,
      // ts из CLI-головы («AGENTS (2026-11-05 19:43): …») — UTC, надёжный
      // порядок; 0 — легаси без времени (не срезается own-фильтром)
      createdAt: parseAgentHeadTs(c.text) ?? 0,
      userId: c.userId,
    })
    const parentHead = c.text.slice(0, 60)
    for (const rep of c.replies) {
      const repCreated = parseAgentHeadTs(rep.text)
      out.push({
        id: rep.id,
        text: `(тред-ответ на «${parentHead}…») ${rep.text}`,
        createdAt: repCreated ?? (out[out.length - 1].createdAt + 1),
        userId: rep.userId,
      })
    }
  }
  return out
}

async function commentTask(bodyDoc: string, text: string): Promise<boolean> {
  if (bodyDoc === "") return false
  if (DRY_RUN) {
    log(`DRY_RUN: comment → ${bodyDoc.slice(0, 8)}: ${text.slice(0, 80)}`)
    return true
  }
  try {
    await mcp.tool("create_comment", { docId: bodyDoc, content: text })
    return true
  } catch (e) {
    log(`comment: ${e}`)
    return false
  }
}

// ——— громкий флаг доски без сессионных колонок (ADR-054) ———

const SESSION_CELLS = new Set([DEV_SESSION_CELL, REVIEW_SESSION_CELL, TEST_SESSION_CELL])

/** Доски, чей флаг отсутствия сессионных колонок уже доставлен в задачу:
 *  один коммент на доску за жизнь процесса — рестарт-спавны не спамят;
 *  недоставка снимает дедуп, следующий спавн ретраит. */
const sessionCellFlagged = new Set<string>()

/** Конфиг-расхождение доски (ADR-054): сессионная ячейка ушла в no-op при
 *  непустой записи — привязка сессии потеряна, фабрика не видит живой заявки
 *  и спавнит дубль. Коммент в задачу — один раз на доску (дедуп выше);
 *  прочий skipped (Branch/Path, пустые очистки) — механика, только лог. */
async function flagMissingSessionCells(
  board: BoardIds, bodyDoc: string, taskTitle: string, cells: Record<string, string>, skipped: string[],
): Promise<void> {
  if (skipped.length === 0) return
  const lost = skipped.filter((name) => SESSION_CELLS.has(name) && (cells[name] ?? "") !== "")
  if (lost.length === 0) {
    log(`доска ${board.dbId.slice(0, 8)}: нет колонок ${skipped.join(", ")} — запись ушла в no-op; сессионных среди них нет, флаг не поднимался`)
    return
  }
  log(`доска ${board.dbId.slice(0, 8)}: сессионных колонок нет (${lost.join(", ")}) — привязка потеряна, возможны дубль-агенты`)
  if (bodyDoc === "" || sessionCellFlagged.has(board.dbId)) return
  sessionCellFlagged.add(board.dbId)
  const delivered = await commentTask(
    bodyDoc,
    `[factory] на доске нет колонок: ${lost.join(", ")} — привязка сессии при спавне потеряна (возможны дубль-агенты). ` +
    `Добавь text-колонку на доску (ADR-054). Задача: ${taskTitle}`,
  )
  if (delivered) log(`флаг сессионных колонок доставлен в задачу ${bodyDoc.slice(0, 8)}`)
  else {
    sessionCellFlagged.delete(board.dbId)
    log(`флаг сессионных колонок не доставлен (${bodyDoc.slice(0, 8)}) — ретрай следующим спавном`)
  }
}

/** Запись сессионной ячейки спавна + громкий флаг при no-op (ADR-054):
 *  общий хвост dev/review/test спавнов — ячейка может молча потеряться,
 *  если колонки на доске нет. */
async function claimSessionCell(
  board: BoardIds, rowId: string, bodyDoc: string, taskTitle: string, cells: Record<string, string>,
): Promise<void> {
  const skipped = await setCellsSafe(board, rowId, cells)
  await flagMissingSessionCells(board, bodyDoc, taskTitle, cells, skipped)
}

// ——— opencode REST (service.json) ———

async function restCall(path: string, init?: { method?: string; body?: unknown }): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = `Basic ${Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")}`
  let res: Response
  try {
    res = await fetch(`${base}/api${path}`, {
      method: init?.method ?? "GET",
      headers: { authorization: auth, "content-type": "application/json" },
      body: init?.body === undefined ? undefined : JSON.stringify(init.body),
      signal: AbortSignal.timeout(REST_TIMEOUT_MS),
    })
  } catch (e) {
    throw Object.assign(new Error(`transport: ${e}`), { status: 0 })
  }
  const text = await res.text()
  if (!res.ok) {
    throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
  }
  if (text.trim() === "") return {}
  const j: unknown = JSON.parse(text)
  return (j as { data?: unknown }).data ?? j
}

interface SessionSpec {
  directory: string
  title: string
  agent?: string
  /** Model.Ref (opencode /api/session): id + providerID обязательны, variant
   *  опционален («provider/model#variant» в объектной форме) */
  model?: { providerID: string; id: string; variant?: string }
}

async function createSession(spec: SessionSpec): Promise<string | null> {
  const raw = await restCall("/session", {
    method: "POST",
    body: {
      title: spec.title.replace(/[\r\n]+/g, " ").slice(0, 64),
      model: spec.model ?? MODEL,
      location: { directory: spec.directory },
      ...(spec.agent !== undefined ? { agent: spec.agent } : {}),
    },
  })
  const id = raw !== null && typeof raw === "object" ? (raw as { id?: unknown }).id : undefined
  return typeof id === "string" && id.startsWith("ses_") ? id : null
}

async function promptSession(sessionID: string, text: string): Promise<boolean> {
  if (DRY_RUN) {
    log(`DRY_RUN: prompt → ${sessionID.slice(0, 16)}: ${text.slice(0, 80)}…`)
    return true
  }
  try {
    await restCall(`/session/${sessionID}/prompt`, { method: "POST", body: { text } })
    return true
  } catch (e) {
    log(`prompt ${sessionID}: ${e}`)
    return false
  }
}

async function sessionLiveness(sessionID: string): Promise<SessionLiveness> {
  if (sessionID === "") return "dead"
  try {
    await restCall(`/session/${sessionID}`)
    return "alive"
  } catch (e) {
    const status = (e as { status?: number }).status
    if (status === 404 || status === 400) return "dead"
    return "unknown"
  }
}

/** Последняя активность сессии: max(created, streamed, completed) последнего
 *  сообщения (DESC limit=1). null — сообщений нет; "invalid" — сбой REST или
 *  нераспознанный формат: пропуск тика (транзиентные сбои и смена формата
 *  /message при апгрейде opencode не дают ложных сигналов). */
async function lastSessionActivity(sessionID: string): Promise<number | null | "invalid"> {
  try {
    const raw = await restCall(`/session/${sessionID}/message?limit=1&order=desc`)
    if (!Array.isArray(raw)) return "invalid"
    return lastActivityOf(raw)
  } catch {
    return "invalid"
  }
}

/** updatedAt тела (ISO) или null. */
async function bodyUpdatedAt(bodyDoc: string): Promise<string | null> {
  if (bodyDoc === "") return null
  try {
    const r = await mcp.tool<{ updatedAt?: string }>("get_doc", { docId: bodyDoc })
    return r.updatedAt ?? null
  } catch {
    return null
  }
}

/** Markdown-экспорт тела; null — экспорт не удался (гейты деградируют к будке —
 *  тихий скип на сбое чтения потерял бы реальную правку владельца).
 *  exportCache — per-tick мемоизация: тик читает тело один раз независимо от
 *  числа потребителей (комменты, хэш, bodyEdit-гейт). */
async function bodyExport(bodyDoc: string, exportCache?: Map<string, string | null>): Promise<string | null> {
  if (bodyDoc === "") return null
  if (exportCache?.has(bodyDoc)) return exportCache.get(bodyDoc) ?? null
  let result: string | null
  try {
    const r = await mcp.tool<{ markdown?: string; exists?: boolean }>("export_doc_markdown", { docId: bodyDoc })
    result = r.exists === true ? r.markdown ?? "" : null
  } catch {
    result = null
  }
  if (exportCache !== undefined) exportCache.set(bodyDoc, result)
  return result
}

/** Хэш норм-формы контента тела вне коммент-записей; null при сбое экспорта. */
async function bodyHashOf(bodyDoc: string, exportCache?: Map<string, string | null>): Promise<string | null> {
  const md = await bodyExport(bodyDoc, exportCache)
  return md === null ? null : bodyContentHash(md)
}

// ——— жизненный цикл задачи ———

/** PR ветки задачи конфликтует с master (gh pr view). GitHub вычисляет
 *  mergeable лениво: первый запрос по «холодному» PR отдаёт UNKNOWN и лишь
 *  запускает фоновое вычисление — опрос повторяется, пока не появится
 *  вердикт или не исчерпаются попытки (канонический сценарий: PR стоит со
 *  вчерашнего ревью, владелец флипает статус — без повтора конфликт-kickoff
 *  молча деградировал бы в generic именно в главном случае). Пустой PR,
 *  DRY_RUN, сбой gh или UNKNOWN после всех попыток — «не конфликт»: путь
 *  будильника не зависит от gh, конфликт агент увидит сам при пуше. */
const MERGEABLE_POLL_ATTEMPTS = 4
const MERGEABLE_POLL_DELAY_MS = 5_000

async function prConflicted(pr: string, cwd: string): Promise<boolean> {
  if (pr === "") return false
  if (DRY_RUN) {
    log(`DRY_RUN: gh pr view ${pr.slice(0, 40)} — mergeable не проверяется`)
    return false
  }
  for (let attempt = 0; attempt < MERGEABLE_POLL_ATTEMPTS; attempt++) {
    let step: MergeablePollStep
    try {
      const raw = await sh(["gh", "pr", "view", pr, "--json", "mergeable"], { cwd })
      step = mergeablePollStep(prMergeableOf(raw), attempt, MERGEABLE_POLL_ATTEMPTS)
    } catch (e) {
      log(`prConflicted ${pr.slice(0, 40)}: ${errText(e)}`)
      return false
    }
    if (step.t === "conflict") return true
    if (step.t === "clear") return false
    if (step.t === "retry") await new Promise((r) => setTimeout(r, MERGEABLE_POLL_DELAY_MS))
  }
  log(`prConflicted ${pr.slice(0, 40)}: mergeable=UNKNOWN после ${MERGEABLE_POLL_ATTEMPTS} попыток — generic-ветка`)
  return false
}

// ——— инит-данные первого промпта (юзер/проект/задача) ———

/** Канон-четвёрка доков проекта: hpath-сегменты под папкой проекта
 *  (слаг для агента — «<proj>/<сегмент>»). */
const PROJECT_INIT_DOCS = ["README", "Домены", "Стек и архитектура", "Деплоймент"]

/** Идентификаторы контуров инита: имена топ-папок вики. */
const PERSON_NOTEBOOK = process.env.UWUWU_PERSON_NOTEBOOK ?? "Experience"
const PROJECTS_NOTEBOOK = "Projects"

/** Инит-блок спавна (постановка: фабрика знает юзер/проект/задачу ДО
 *  спавна — агент не тратит ход на person show/доки/task get ради
 *  ориентации). Деградация любого источника честная (null/пустой список
 *  + строка в блоке), спавн от полноты инита не зависит. */
async function fetchSpawnInit(projectName: string, cfg: ProjectCfg): Promise<Omit<SpawnInit, "task">> {
  // резолв ноутбуков — под тем же контрактом честной деградации, что и
  // источники: сбой одного kernel-вызова не фейлит спавн (фетчеры увидят
  // пустой id и отлогируют пропуск своего контура)
  let notebookIds: Record<string, string> = {}
  try {
    notebookIds = (await mcp.tool<{ ids?: Record<string, string> }>("notebook_ids_by_names", {
      names: [PERSON_NOTEBOOK, PROJECTS_NOTEBOOK],
    })).ids ?? {}
  } catch (e) {
    log(`инит: резолв ноутбуков не удался (${errText(e)})`)
  }
  const [person, projectDocs] = await Promise.all([
    fetchPersonMarkdown(notebookIds[PERSON_NOTEBOOK] ?? ""),
    fetchProjectDocSlugs(projectName, notebookIds[PROJECTS_NOTEBOOK] ?? ""),
  ])
  return {
    person,
    projectDocs,
    // env-задачам путь не нужен — repo/скретч называет env-строка промпта;
    // буква постановки «проект: путь» — для env:false (агент сам создаёт ворктри)
    ...(cfg.env === false ? { localPath: cfg.local } : {}),
  }
}

/** Персона владельца: Experience:/personal/PERSON (env-шов паритета CLI).
 *  null — дока нет, дубль hpath (≥2 id — не угадываем, паритет person show)
 *  или сбой ядра: во всех случаях пропуск источника, не ошибка спавна. */
async function fetchPersonMarkdown(personNotebookId: string): Promise<string | null> {
  try {
    const hpath = process.env.UWUWU_PERSON_HPATH ?? "/personal/PERSON"
    if (personNotebookId === "") {
      log(`инит: ноутбук «${PERSON_NOTEBOOK}» не найден — персона пропущена`)
      return null
    }
    const ids = (await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", { notebook: personNotebookId, path: hpath })).ids ?? []
    if (ids.length !== 1) {
      log(`инит: персона ${ids.length === 0 ? "не найдена" : `неоднозначна (${ids.length} id)`} — пропущена`)
      return null
    }
    const doc = await mcp.tool<{ markdown?: string; exists?: boolean; title?: string }>("export_doc_markdown", { docId: ids[0] as string })
    const md = stripExportWrapper(doc.exists === true ? doc.markdown ?? "" : "", doc.title ?? "").trim()
    if (md === "") {
      log("инит: экспорт персоны пуст — пропущена")
      return null
    }
    return md
  } catch (e) {
    log(`инит: персона не получена (${errText(e)}) — пропущена`)
    return null
  }
}

/** Слаги живых доков канон-четвёрки проекта (uwuwu-cli read «<proj>/<док>»).
 *  Мёртвый/неоднозначный hpath — док пропускается, не угадываем; четвёрка
 *  резолвится параллельно (порядок слагов — порядок PROJECT_INIT_DOCS). */
async function fetchProjectDocSlugs(projectName: string, projectsNotebookId: string): Promise<string[]> {
  try {
    if (projectsNotebookId === "") {
      log(`инит: ноутбук «${PROJECTS_NOTEBOOK}» не найден — доки проекта пропущены`)
      return []
    }
    const resolved = await Promise.all(PROJECT_INIT_DOCS.map(async (title) => ({
      title,
      ids: (await mcp.tool<{ ids?: string[] }>("doc_ids_by_hpath", {
        notebook: projectsNotebookId, path: `/${projectName}/${title}`,
      })).ids ?? [],
    })))
    const slugs: string[] = []
    for (const { title, ids } of resolved) {
      if (ids.length === 1) slugs.push(`${projectName}/${title}`)
      else log(`инит: док «${projectName}/${title}» ${ids.length === 0 ? "не найден" : `неоднозначен (${ids.length} id)`} — пропущен`)
    }
    return slugs
  } catch (e) {
    log(`инит: доки проекта не резолвятся (${errText(e)}) — пропущены`)
    return []
  }
}

async function startTask(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState,
  attemptsSeed: number, start: StartKind, counters?: PassCounters,
): Promise<void> {
  // стоп-сигнал: не спавним новую работу — задача доедет при следующем старте
  if (drain.isStopping()) return
  // DRY_RUN: решение о спавне принято (routeTask отлогировал) — сессию не создаём
  if (DRY_RUN) {
    log(`DRY_RUN: спавн девелопера над ${task.rowId.slice(0, 8)} (${start}) — пропущен`)
    return
  }
  // env-механика: work/task-ветку и ворктри делает фабрика, сессия стартует
  // cwd=repo. env:false-проект → поведение вне механики (агент сам)
  const useEnv = cfg.env !== false
  let envCtx: SpawnEnvContext | undefined
  let envDir: string | undefined
  if (useEnv) {
    const paths = await ensureEnv(envDeps(), { root: ENV_ROOT, rowId: task.rowId, local: cfg.local, base: baseOf(cfg) })
    envCtx = {
      repoDir: paths.repoDir,
      branch: paths.branch,
      scratchDir: paths.scratchDir,
      ...(paths.display !== null ? { display: { num: paths.display, file: paths.displayFile } } : {}),
    }
    envDir = paths.envDir
    if (counters !== undefined) counters.envCreated++
  }

  // тайтл: линкованная строка несёт заголовок в доке-теле — деривируем из него
  let title = task.title
  if (title === "" && task.bodyDoc !== "") {
    const doc = await mcp.tool<{ title?: string }>("get_doc", { docId: task.bodyDoc })
    title = (doc.title ?? "").trim()
  }
  if (title === "") title = `задача ${task.rowId.slice(0, 8)}`

  // инит-блок первого промпта: юзер/проект/задача до старта сессии
  // (деградация внутри fetchSpawnInit — спавн не зависит от полноты инита)
  const initBase = await fetchSpawnInit(projectName, cfg)
  const init: SpawnInit = {
    ...initBase,
    task: { title, status: task.status, type: task.type, branch: task.branch, pr: task.pr },
  }

  // имя сессии «проект: тайтл» — осмысленно в списке сессий опенкода
  const sessionID = await createSession({
    directory: envCtx?.repoDir ?? cfg.local,
    title: `📋 ${projectName}: ${title}`,
  })
  if (sessionID === null) throw new Error("сессия не создалась")

  const sent = await promptSession(sessionID, promptForStart(
    start,
    { rowId: task.rowId, project: projectName, repo: cfg.repo, pr: task.pr },
    // envRoot — даже при envCtx=undefined (env:false): промпт называет агенту
    // канон-путь ворктри, который потом резолвит resolvePromptWorktree
    { env: envCtx, init, envRoot: ENV_ROOT },
  ))
  if (!sent) throw new Error("spawn-промпт не ушёл")

  // живость сразу после спавна: «фиктивные sessionID» (create ок, сессии
  // нет) не попадают в state и не считаются активными — throw уводит в
  // registerFailure, ретрай следующим тиком; осиротевшую сессию гасим
  const liveness = await sessionLiveness(sessionID)
  if (liveness === "dead") {
    await restCall(`/session/${sessionID}`, { method: "DELETE" }).catch(() => {})
    throw new Error("сессия мертва сразу после спавна")
  }
  // unknown (транзиент REST) — пишем state как раньше: stage-2 перепроверит

  // граница комментов ДО старта — не будить агента старыми;
  // маркер = последний юзерский коммент (позиционный поиск в
  // newUserComments идёт по юзерскому списку)
  const startExports = new Map<string, string | null>()
  const last = latestComment(
    (await taskComments(task.bodyDoc, startExports)).filter((c) => isUserComment(c, BOT_USER_ID)),
  )

  const entry: TrackedTask = {
    sessionID,
    worktree: envCtx?.repoDir ?? "",
    branch: envCtx?.branch ?? "",
    bodyDoc: task.bodyDoc,
    title,
    lastCommentAt: last?.createdAt ?? 0,
    lastCommentId: last?.id ?? "",
    createdAt: Date.now(),
    attempts: attemptsSeed,
    project: projectName,
    bodyUpdated: await bodyUpdatedAt(task.bodyDoc) ?? undefined,
    bodyHash: (await bodyHashOf(task.bodyDoc, startExports)) ?? undefined,
    agentActive: true, // spawn отправлен — агент работает
    // спавн-промпт = первый сигнал сессии: часы тишины сталла отсчитываются
    // от него, пока агент не породил активность
    lastDevPromptAt: Date.now(),
    lastStatus: task.status, // спавн из этого статуса (Design/In Progress) — не рестартить им же
    priority: task.priority,
    ...(envDir !== undefined ? { envDir } : {}),
    // история корней: append при каждом спавне (рестарты не теряют прошлые
    // поколения сессий — BFS задачного прогона накрывает осиротевших)
    sessionHistory: [...(state.tasks[task.rowId]?.sessionHistory ?? []), sessionID],
  }
  state.tasks[task.rowId] = entry
  saveState(state)
  const spawnCells = {
    ...(envCtx !== undefined
      ? { Branch: envCtx.branch, [PATH_CELL]: envCtx.repoDir }
      : {}),
    [DEV_SESSION_CELL]: sessionID,
  }
  await claimSessionCell(board, task.rowId, task.bodyDoc, title, spawnCells)
  writeTaskStartSignal(task.rowId, sessionID)
  log(`задача ${task.rowId.slice(0, 8)}: сессия ${sessionID}, старт ${start}${envCtx !== undefined ? `, env=${envDir}` : " (env:false — агент сам делает ворктри)"} (попыток до этого: ${attemptsSeed})`)
}

/** GC легаси-корней ворктри проекта (wt/, .worktrees/ под cfg.local):
 *  снос merged+clean по факту смержа PR с полным набором гардов.
 *  Проекты без легаси-корней — мгновенный no-op внутри gcLegacyWorktrees. */
async function gcLegacyRoots(name: string, cfg: ProjectCfg, state: FactoryState): Promise<void> {
  const guards = activeTaskGuards(state)
  const report = await gcLegacyWorktrees(envDeps(), {
    local: cfg.local,
    roots: [join(cfg.local, "wt"), join(cfg.local, ".worktrees")],
    base: baseOf(cfg),
    protectedBranches: guards.branches,
    protectedDirs: guards.dirs,
    nowMs: Date.now(),
    graceMs: MERGE_GRACE_MS_DEFAULT,
  })
  log(
    `gc[${name}]: снесено=${report.removed.length}, веток=${report.branchesDeleted.length}, ` +
      `пропущено=${report.skipped.length}, неудач=${report.failed.length}` +
      (report.abortedReason !== undefined ? `, прерван: ${report.abortedReason}` : ""),
  )
}

/** Чистка state (Done): гасим ОБЕ сессии задачи и сносим env (rm/attic +
 *  prune + branch -d). Сессии задачи НЕ удаляются из opencode.db (решение
 *  владельца 08.11: переоткрытие задачи должно находить все сессии; чистку
 *  БД не делаем ни фабрике при Done, ни дримеру после прогона) — вместо
 *  терминирования в state дримера уходит done-сигнал: задачный прогон
 *  пост-обработает пакет сессий задачи. */
async function finishTask(
  rowId: string, tracked: TrackedTask, cfg: ProjectCfg, state: FactoryState, counters?: PassCounters,
): Promise<void> {
  // DRY_RUN: чистка в песочный state безвредна, но done-сигнал и снос
  // реального env — side-effect на прод-сервере — не делаем
  if (DRY_RUN) {
    log(`DRY_RUN: Done-чистка ${rowId.slice(0, 8)} — state-запись удалена бы, done-сигнал не пишется, env не сносится`)
    delete state.tasks[rowId]
    saveState(state)
    return
  }
  log(`задача ${rowId.slice(0, 8)}: Done — done-сигнал дримеру + чистка state`)
  // запись сигнала обязательна: при сбое (диск/права) чистка откладывается —
  // трек жив, Done-строка в скане → ретрай finishTask следующим тиком.
  // Удаление state без сигнала = тихая полная потеря пост-обработки задачи
  // (сессии остались бы и в активном наборе дримера — посессионный пул
  // скипал бы их вечно).
  if (!writeTaskDoneSignal(rowId, tracked, cfg)) {
    log(`задача ${rowId.slice(0, 8)}: done-сигнал не записан — чистка отложена, ретрай следующим тиком`)
    return
  }
  const hasEnv = tracked.envDir !== undefined || tracked.worktree !== ""
  if (hasEnv) {
    const outcome = await cleanupEnv(envDeps(), {
      root: ENV_ROOT, rowId, local: cfg.local,
      envDir: tracked.envDir, branch: tracked.branch !== "" ? tracked.branch : undefined,
    })
    if (counters !== undefined) {
      if (outcome === "removed") counters.envRemoved++
      else if (outcome === "atticked") counters.envAtticked++
    }
    if (outcome === "atticked") {
      await notify(tracked,
        `[factory] Задача закрыта, но в окружении осталось незакоммиченное — env переехал в attic (${ENV_ROOT}/.attic), тяжёлые каталоги вырезаны. Разобрать или дослить — руками.`)
    } else if (outcome === "failed") {
      await notify(tracked,
        `[factory] Задача закрыта, но снос окружения не удался — каталог оставлен как есть, разбори ${ENV_ROOT} руками.`)
    }
  }
  delete state.tasks[rowId]
  saveState(state)
}

// ——— очередь сигналов дримеру (start/done) ———

/** Каталог очереди дримера: дефолт — state дримера на той же ноде; env
 *  UWUWU_DREAMER_QUEUE_DIR общий с демоном (он же читатель) — тесты уводят
 *  обе стороны в одну tmp-песочницу. */
const DREAMER_QUEUE_DIR = process.env.UWUWU_DREAMER_QUEUE_DIR
  ?? join(homedir(), ".local", "state", "uwuwu-dreamer", "queue")

/** Трек задачи для очереди-сигналов: пишущий ход обязан передать запись,
 *  не hope-читать по key после перезаписи entry. */
type QueueSignalTask = Pick<TrackedTask,
  "project" | "title" | "envDir" | "sessionHistory" | "sessionID"
  | "reviewSessionID" | "testSessionID" | "branch" | "mergedPrUrl">

/** Атомарная запись файла очереди (tmp + rename): дример забирает готовый
 *  JSON, полуфайлов не видит. true = записан; false = сбой (диск/права) —
 *  вызывающий решает (done — чистка откладывается, start — деградация к
 *  посессионной обработке). */
function writeQueueSignal(fileName: string, payload: Record<string, unknown>): boolean {
  try {
    mkdirSync(DREAMER_QUEUE_DIR, { recursive: true })
    const tmp = join(DREAMER_QUEUE_DIR, `${fileName}.tmp`)
    writeFileSync(tmp, JSON.stringify(payload))
    renameSync(tmp, join(DREAMER_QUEUE_DIR, fileName))
    log(`очередь дримера: ${fileName} записан`)
    return true
  } catch (e) {
    log(`очередь дримера: ${fileName} не записан — ${e}`)
    return false
  }
}

/** Start-сигнал: сессия задачи известна активному набору дримера —
 *  посессионный контур скипает её до закрытия задачи (все проекты единообразно). */
function writeTaskStartSignal(rowId: string, sessionID: string): void {
  writeQueueSignal(`start-${rowId}.json`, { rowId, sessionID, ts: Date.now() })
}

/** Done-сигнал: постановка задачного прогона. rootSessions — полная история
 *  корней (sessionHistory + текущие id легаси-треков без истории).
 *  true = записан; false — вызывающий обязан отложить чистку (см. finishTask). */
function writeTaskDoneSignal(rowId: string, tracked: QueueSignalTask, cfg: ProjectCfg): boolean {
  const rootSessions = [...new Set([
    ...(tracked.sessionHistory ?? []),
    tracked.sessionID,
    tracked.reviewSessionID ?? "",
    tracked.testSessionID ?? "",
  ].filter((s) => s.startsWith("ses_")))]
  return writeQueueSignal(`done-${rowId}.json`, {
    rowId, project: tracked.project, title: tracked.title,
    rootSessions,
    branch: tracked.branch, pr: mergedPrUrlOf(tracked),
    envDir: tracked.envDir ?? "", directory: cfg.local,
    closedAt: Date.now(),
  })
}

/** PR задачи: кэш смерженного приоритетнее ячейки (ячейку мог очистить
 *  Done-проход фабрики). */
function mergedPrUrlOf(tracked: QueueSignalTask): string {
  return tracked.mergedPrUrl ?? ""
}

async function notify(task: TrackedTask, body: string): Promise<void> {
  // один ретрай: потеря коммента недопустима
  if (await commentTask(task.bodyDoc, body)) return
  await new Promise((r) => setTimeout(r, 15_000))
  if (!(await commentTask(task.bodyDoc, body))) log(`уведомление не доставлено: ${body.slice(0, 80)}`)
}

/** Снимок state перед перезаписью из подозрительно пустого скана (паритет
 *  с двухпроходной чисткой трекинга, ADR-031): датированный, обычным .bak
 *  не ротируется. */
function backupSuspectState(): void {
  if (!existsSync(STATE_FILE)) return
  const stamp = new Date().toISOString().replaceAll(":", "-")
  try {
    copyFileSync(STATE_FILE, `${STATE_FILE}.bak-suspect-${stamp}`)
    log(`гард спавна: бэкап state → ${STATE_FILE}.bak-suspect-${stamp}`)
  } catch (e) {
    log(`гард спавна: бэкап не удался — ${errText(e)}`)
  }
}

/** HOLD-тик гарда каскада: спавны отложены (обслуживание существующих
 *  треков продолжается — гард бьёт только по новому спавну). Алерт-политика:
 *  first — вход в эпизод (бэкап + коммент владельцу), repeat — длящийся
 *  HOLD за окном повтора (коммент снова), none — в пределах окна (только
 *  лог). Флаг эпизода ставится по факту ПОПЫТКИ алерта, не доставки:
 *  недоставимый алерт не должен ретраиться каждый тик — окно повтора
 *  вернётся к нему через час. */
async function holdSpawns(
  guard: SpawnGuardHold, untracked: TaskRef[], orphaned: number,
  state: FactoryState, counters: PassCounters,
): Promise<void> {
  const targets = untracked.slice(0, 10).map((r) => r.rowId.slice(0, 8))
  counters.held = untracked.length
  counters.heldReason = guard.reason
  log(`гард спавна: HOLD (${guard.reason}), сирот=${orphaned}, untracked=${untracked.length}` +
    ` — спавны отложены: ${targets.join(", ") || "нет"}`)
  if (guard.alert === "none") return
  if (guard.alert === "first") backupSuspectState()
  const target = alertTargetOf(untracked)
  if (target === null) {
    log("гард спавна: адресата алерта нет (ни одной строки с телом) — только лог")
  } else if (await commentTask(target.bodyDoc, guard.text)) {
    log(`гард спавна: алерт доставлен в задачу ${target.rowId.slice(0, 8)}`)
  } else {
    log(`гард спавна: алерт в ${target.rowId.slice(0, 8)} не доставлен — повтор в пределах окна`)
  }
  state.holdSignaled = new Date().toISOString()
  saveState(state)
}

async function registerFailure(
  task: TaskRef, projectName: string, board: BoardIds, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  const prev = state.tasks[task.rowId]
  const entry: TrackedTask = prev ?? {
    sessionID: "", worktree: "", branch: "",
    bodyDoc: task.bodyDoc, title: task.title, project: projectName,
    lastCommentAt: 0, lastCommentId: "", createdAt: Date.now(), attempts: 0,
  }
  // без этого чужой тик считал бы запись своей потерянной («строка исчезла»)
  // и стирал её — бесконечный цикл стартов вместо Blocked
  if (entry.project === "") entry.project = projectName
  entry.attempts = attempts
  state.tasks[task.rowId] = entry
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    // единый статус «ждёт юзера»: не важно — агент вопрос задал или
    // демон не смог стартовать; реакция юзера одна (коммент в задачу)
    await setCells(board, task.rowId, { Status: "Needs attention" })
    await notify(entry,
      `[factory] не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH} и напиши комментарий — попробую снова.`)
  }
}

/** Пережить агентские сессии задачи при замене записи state (рестарт
 *  девелопера): инвариант 1 задача = 1 ревьювер и 1 тестировщик — их сессии
 *  не пересоздаются, продолжение через rerun-промпты. */
function preserveAgentSessions(from: TrackedTask, to: TrackedTask): void {
  if (from.reviewSessionID !== undefined) {
    to.reviewSessionID = from.reviewSessionID
    to.reviewAttempts = from.reviewAttempts
    to.reviewKind = from.reviewKind
  }
  if (from.testSessionID !== undefined) {
    to.testSessionID = from.testSessionID
    to.testAttempts = from.testAttempts
  }
}

async function restartTask(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState, fresh: TaskComment[],
): Promise<void> {
  const tracked = state.tasks[task.rowId]
  if (tracked === undefined) return
  const seed = tracked.attempts
  const last = fresh[fresh.length - 1]
  tracked.lastCommentAt = last.createdAt
  tracked.lastCommentId = last.id
  tracked.sessionID = "" // карантин до успеха старта
  saveState(state)
  try {
    await startTask(task, projectName, cfg, board, state, seed, "continue")
    const reborn = state.tasks[task.rowId]
    if (reborn !== undefined) {
      preserveAgentSessions(tracked, reborn)
      saveState(state)
    }
  } catch (e) {
    await registerFailure(task, projectName, board, state, seed, e)
  }
}

// ——— ревьювер: спавн/будильник на ревью-статусах ———

interface PrProbeOutcome { result: PrProbeResult; mergedUrl: string | null }

/** Состояние PR по URL из PR-ячейки (gh — прямая истина о мерже). Сбой пробы
 *  (не-GitHub проект, gh недоступен/неавторизован) — unknown: сталл-ветка
 *  отложит кик, предел деградации в prProbeVerdict вернёт страховку. */
async function prProbeByCell(url: string): Promise<PrProbeOutcome> {
  try {
    const state = await sh(["gh", "pr", "view", url, "--json", "state", "--jq", ".state"])
    return state === "MERGED" ? { result: "merged", mergedUrl: url } : { result: "open", mergedUrl: null }
  } catch { return { result: "unknown", mergedUrl: null } }
}

/** Состояние PR по ветке — фоллбек при пустой PR-ячейке (инцидент 09.11:
 *  задача 0m2net0 с мерженным PR не была защищена пробой — клетка пуста на
 *  момент чтения). Репо — явно (cwd фабрики — чужой чекаут). */
async function prProbeByBranch(repo: string, branch: string): Promise<PrProbeOutcome> {
  try {
    const url = await sh([
      "gh", "pr", "list", "-R", repo, "--head", branch, "--state", "merged",
      "--json", "url", "--jq", ".[0].url",
    ])
    return url !== "" ? { result: "merged", mergedUrl: url } : { result: "open", mergedUrl: null }
  } catch { return { result: "unknown", mergedUrl: null } }
}

// ——— автодон: галочка владельца → проба/мерж PR на входе в Review ———
// (задача 20261112160316; решение одного тика — autodoneAction в autodone.ts)

/** Токен tea-логина по имени-хосту (конфиг ~/.config/tea/config.yml).
 *  null — логина/токена нет: проба уйдёт в unknown → streak → эскалация,
 *  не тихая деградация. */
function teaTokenFor(host: string): string | null {
  try {
    const raw = readFileSync(join(homedir(), ".config", "tea", "config.yml"), "utf8")
    const blocks = raw.split(/^\s*-\s+name:\s*/m).slice(1)
    for (const block of blocks) {
      const name = block.split("\n")[0]?.trim() ?? ""
      if (name !== host) continue
      const m = block.match(/^\s+token:\s*(\S+)/m)
      return m?.[1] ?? null
    }
    return null
  } catch {
    return null
  }
}

/** Gitea API GET (токен из конфига tea). Сбой/не-2xx → null: проба честно
 *  уходит в unknown (стрик до эскалации), не маскируется. Таймаут — тот же
 *  SPAWN_TIMEOUT_MS (ревью Low: зависший хост не должен висеть на дефолте). */
async function giteaApi(host: string, path: string): Promise<unknown | null> {
  const token = teaTokenFor(host)
  if (token === null) return null
  try {
    const r = await fetch(`https://${host}/api/v1${path}`, {
      headers: { Authorization: `token ${token}` },
      signal: AbortSignal.timeout(SPAWN_TIMEOUT_MS),
    })
    if (!r.ok) return null
    return await r.json()
  } catch {
    return null
  }
}

/** Проба PR по носителю. GitHub — gh pr view; Gitea — HTTP API (pulls/{index}
 *  + combined status head-SHA; пустой статус = CI не настроен, не блокер).
 *  Фоллбек по ветке — поиск открытого PR; не нашли → носитель «без ПР». */
async function autodoneProbe(
  carrier: PrCarrier, cfg: ProjectCfg,
): Promise<{ state: PrProbeState; pr: PrCarrier }> {
  switch (carrier.kind) {
    case "github": {
      try {
        const raw = await sh([
          "gh", "pr", "view", carrier.url,
          "--json", "state,mergeable,mergeStateStatus,statusCheckRollup",
        ])
        return { state: githubProbeVerdict(JSON.parse(raw)), pr: carrier }
      } catch {
        return { state: "unknown", pr: carrier }
      }
    }
    case "gitea": {
      const data = await giteaApi(carrier.host, `/repos/${carrier.slug}/pulls/${carrier.index}`)
      if (data === null || typeof data !== "object") return { state: "unknown", pr: carrier }
      const d = data as { head?: { sha?: string }; state?: string; merged?: boolean; mergeable?: boolean }
      let ci = ""
      if (typeof d.head?.sha === "string" && d.head.sha !== "") {
        const st = await giteaApi(carrier.host, `/repos/${carrier.slug}/commits/${d.head.sha}/status`)
        ci = String((st as { state?: string } | null)?.state ?? "")
      }
      return { state: giteaProbeVerdict(d, ci), pr: carrier }
    }
    case "github-branch": {
      try {
        const url = await sh([
          "gh", "pr", "list", "-R", cfg.repo, "--head", carrier.branch,
          "--state", "open", "--json", "url", "--jq", ".[0].url",
        ])
        if (url === "") return { state: "clean", pr: { kind: "none" } }
        return await autodoneProbe({ kind: "github", url }, cfg)
      } catch {
        return { state: "unknown", pr: carrier }
      }
    }
    case "gitea-branch": {
      const list = await giteaApi(carrier.host, `/repos/${carrier.slug}/pulls?state=open`)
      if (list === null || !Array.isArray(list)) return { state: "unknown", pr: carrier }
      const hit = list.find((p) => (p as { head?: { ref?: string } })?.head?.ref === carrier.branch)
      if (hit === undefined) return { state: "clean", pr: { kind: "none" } }
      const h = hit as { html_url?: string; number?: number }
      return await autodoneProbe({
        kind: "gitea", url: h.html_url ?? "", host: carrier.host,
        index: String(h.number ?? ""), slug: carrier.slug,
      }, cfg)
    }
    case "none":
      return { state: "clean", pr: carrier }
    case "invalid":
      // недостижимо из runAutodoneTick (invalid перехватывается раньше с
      // эскалацией), но тип требует полноты: honest unknown, не падение
      return { state: "unknown", pr: carrier }
  }
}

/** Мерж PR носителя: gh --merge / tea --style merge (merge-commit — как
 *  мержит владелец). false — сбой вызова: streak до эскалации. */
async function mergePr(pr: PrCarrier): Promise<boolean> {
  try {
    if (pr.kind === "github") {
      await sh(["gh", "pr", "merge", pr.url, "--merge"])
      return true
    }
    if (pr.kind === "gitea") {
      await sh(["tea", "pr", "merge", pr.index, "--repo", pr.slug, "--login", pr.host, "--style", "merge"])
      return true
    }
    return false
  } catch (e) {
    log(`autodone merge (${pr.kind}): ${errText(e)}`)
    return false
  }
}

/** Один тик автодона по задаче в Review с галочкой: проба → вердикт →
 *  исход. Все статусы-пишутся через setCells (DRY_RUN гейтится внутри),
 *  merge-вызовы гейтятся явно (новый класс внешних мутаций — замечание
 *  ревью High). Bounce НЕ трогает lastStatus: следующий тик вернёт мяч
 *  девелоперу штатным return-to-dev. */
async function runAutodoneTick(
  task: TaskRef, tracked: TrackedTask, ctx: { cfg: ProjectCfg; board: BoardIds; state: FactoryState; counters: PassCounters },
): Promise<void> {
  const { cfg, board, state, counters } = ctx
  const rowId = task.rowId
  if (tracked.autodoneState !== "pending") {
    tracked.autodoneState = "pending"
    tracked.autodoneSince = Date.now()
    // новый эпизод — свежий бюджет проб: unknown-страйк прошлого эпизода
    // (эскалация на сбойном хосте) не должна эскалировать с первой пробы
    // (ревью раунд 2, Low)
    tracked.prProbeUnknownStreak = undefined
    saveState(state)
    log(`задача ${rowId.slice(0, 8)}: autodone — вход в Review, проба PR`)
  }
  const finishDone = async (comment: string): Promise<void> => {
    await setCells(board, rowId, { Status: "Done" })
    tracked.autodoneState = "merged"
    if (task.pr !== "") tracked.mergedPrUrl = task.pr
    tracked.lastStatus = "Done"
    saveState(state)
    counters.autodoneMerged = (counters.autodoneMerged ?? 0) + 1
    await commentTask(tracked.bodyDoc, `${AGENT_MARK} autodone: ${comment}`)
  }
  const carrier0 = prCarrierOf(task.pr, task.branch, cfg.repo)
  if (carrier0.kind === "none") {
    log(`задача ${rowId.slice(0, 8)}: autodone — PR-ячейки нет (wiki-URL/пусто) → Done`)
    await finishDone("задача без PR закрывается — Done")
    return
  }
  if (carrier0.kind === "invalid") {
    // мусор в PR-ячейке (PR-паттерн без схемы и пр.): громко человеку, не
    // Done и не крэш тика (ревью Common-1)
    log(`задача ${rowId.slice(0, 8)}: autodone — PR-ячейка не парсится как URL → Needs attention`)
    await setCells(board, rowId, { Status: "Needs attention" })
    tracked.autodoneState = "escalated"
    tracked.lastStatus = "Needs attention"
    saveState(state)
    await notify(tracked, `[factory] autodone: PR-ячейка не парсится как URL («${carrier0.url}»). Поправь ячейку и переставь Review — автодон перезапустится.`)
    return
  }
  const { state: probe, pr } = await autodoneProbe(carrier0, cfg)
  if (pr.kind === "none") {
    log(`задача ${rowId.slice(0, 8)}: autodone — PR по ветке не найден → Done`)
    await finishDone("PR по ветке не найден — задача без PR закрывается: Done")
    return
  }
  const pendingMs = Date.now() - (tracked.autodoneSince ?? Date.now())
  const act = autodoneAction(probe, {
    pendingMs,
    timeoutMs: AUTODONE_TIMEOUT_MS,
    unknownStreak: tracked.prProbeUnknownStreak ?? 0,
    unknownMax: PR_PROBE_UNKNOWN_MAX,
  })
  switch (act.t) {
    case "wait": {
      log(`задача ${rowId.slice(0, 8)}: autodone — ${probe}, жду следующий тик (${Math.round(pendingMs / 60_000)} мин в Review)`)
      return
    }
    case "merge": {
      if (DRY_RUN) {
        log(`DRY_RUN: autodone merge (${pr.kind}) по ${rowId.slice(0, 8)} — пропущен`)
        return
      }
      const ok = await mergePr(pr)
      if (!ok) {
        tracked.prProbeUnknownStreak = (tracked.prProbeUnknownStreak ?? 0) + 1
        saveState(state)
        log(`задача ${rowId.slice(0, 8)}: autodone — merge не удался (попытка ${tracked.prProbeUnknownStreak}/${PR_PROBE_UNKNOWN_MAX})`)
        return
      }
      tracked.prProbeUnknownStreak = undefined
      log(`задача ${rowId.slice(0, 8)}: autodone — PR смержен → Done`)
      await finishDone(`PR смержен (${pr.kind}) — Done`)
      return
    }
    case "done": {
      log(`задача ${rowId.slice(0, 8)}: autodone — ${act.reason} → Done`)
      await finishDone(`${act.reason} — Done`)
      return
    }
    case "bounce": {
      log(`задача ${rowId.slice(0, 8)}: autodone — bounce: ${act.reason} → In Progress`)
      await setCells(board, rowId, { Status: "In Progress" })
      tracked.autodoneState = "bounced"
      tracked.prProbeUnknownStreak = undefined
      saveState(state)
      counters.autodoneBounced = (counters.autodoneBounced ?? 0) + 1
      // lastStatus не трогаем — следующий тик вернёт мяч девелоперу
      await commentTask(tracked.bodyDoc, `${AGENT_MARK} autodone: ${act.reason} — задача возвращена в In Progress`)
      return
    }
    case "escalate": {
      log(`задача ${rowId.slice(0, 8)}: autodone — эскалация: ${act.reason}`)
      await setCells(board, rowId, { Status: "Needs attention" })
      tracked.autodoneState = "escalated"
      tracked.lastStatus = "Needs attention"
      saveState(state)
      await notify(tracked, `[factory] autodone: ${act.reason}. Разберись (CI/конфликт/проба) — после разбора проведи задачу через пайп заново или переставь Review: автодон перезапустится на новом входе.`)
      return
    }
  }
}

/** Спавн ревьювера: 1 сессия на задачу. Без parentID: наследование каталога
 *  родителем побеждает явный location (проверено на opencode REST) — а ревьюверу
 *  нужен cwd ворктри. Выбрасывает при фейле. */
async function startReview(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState,
  kind: ReviewKind, tracked: TrackedTask,
): Promise<void> {
  // DRY_RUN: решение о ревью принято — ревьювера не спавним
  if (DRY_RUN) {
    log(`DRY_RUN: спавн ревьювера (${kind}) над ${task.rowId.slice(0, 8)} — пропущен`)
    return
  }
  // ворктри задачи: state-путь фабрики (env:true) или живой агентский
  // ворктри env:false по канон-пути (девелопер создал сам); оба пусты
  // (карантинный ревью-вход без дев-спавна) → честная строка промпта
  // «ревью по diff/PR» из каталога сессии cfg.local
  const worktree = resolvePromptWorktree(task.rowId, tracked.worktree, ENV_ROOT)
  const ctx = { branch: task.branch, worktree, pr: task.pr, type: task.type, base: baseOf(cfg) }
  const reviewer = kind === "plan" ? PLAN_REVIEWER : REVIEWER
  const sessionID = await createSession({
    directory: worktree !== "" ? worktree : cfg.local,
    title: `🔍 ${projectName}: ревью — ${tracked.title || task.title}`,
    agent: reviewer.agent,
    model: reviewer.model,
  })
  if (sessionID === null) throw new Error("ревью-сессия не создалась")
  const sent = await promptSession(sessionID, reviewSpawnPrompt(task.rowId, projectName, kind, ctx, reviewer.agent))
  if (!sent) {
    await restCall(`/session/${sessionID}`, { method: "DELETE" }).catch(() => {})
    throw new Error("ревью-промпт не ушёл")
  }
  tracked.reviewSessionID = sessionID
  tracked.reviewAttempts = 0
  tracked.reviewKind = kind
  // история корней: ревьювер — корневая сессия задачи, дример должен её видеть
  tracked.sessionHistory = [...(tracked.sessionHistory ?? []), sessionID]
  saveState(state)
  await claimSessionCell(board, task.rowId, task.bodyDoc, tracked.title || task.title, { [REVIEW_SESSION_CELL]: sessionID })
  writeTaskStartSignal(task.rowId, sessionID)
  log(`задача ${task.rowId.slice(0, 8)}: ревью-сессия ${sessionID} (${kind})`)
}

/** Эскалация фейлов спавна ревьювера: 3 подряд → Needs attention. */
async function failReview(
  task: TaskRef, board: BoardIds, tracked: TrackedTask, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  tracked.reviewAttempts = attempts
  tracked.reviewSessionID = ""
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: ревью-старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    await setCells(board, task.rowId, { Status: "Needs attention" })
    // lastStatus — сразу за доской: если юзер переставит ревью-статус быстрее
    // тика, entering-проверка всё равно посчитает это новым входом (seed=0)
    tracked.lastStatus = "Needs attention"
    saveState(state)
    await notify(tracked,
      `[factory] ревьювер не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH}; ` +
      "чтобы перезапустить ревью — переставь ревью-статус (счётчик попыток обнулится).")
  }
}

/** Лиз Xvfb-дисплея задачи из tracked.envDir (лиз-файл ведёт ensureEnv):
 *  файл есть и парсится → lease; иначе undefined — строка DISPLAY в
 *  промпт не попадает (насыщение, старый env до фичи). */
function displayLeaseOf(tracked: TrackedTask): DisplayLease | undefined {
  if (tracked.envDir === undefined) return undefined
  const file = displayFilePath(tracked.envDir)
  const num = readDisplayNumber(file)
  return num !== null ? { num, file } : undefined
}

/** Спавн тестировщика: 1 сессия на задачу (cwd = ворктри задачи — там код,
 *  который он гоняет и допокрывает тестами). Выбрасывает при фейле. */
async function startTesting(
  task: TaskRef, projectName: string, cfg: ProjectCfg, board: BoardIds, state: FactoryState,
  tracked: TrackedTask,
): Promise<void> {
  if (DRY_RUN) {
    log(`DRY_RUN: спавн тестировщика над ${task.rowId.slice(0, 8)} — пропущен`)
    return
  }
  // ворктри для ctx/cwd: state-путь фабрики или живой агентский env:false
  // (см. resolvePromptWorktree) — промпт не лжёт «не найден» при живом ворктри
  const worktree = resolvePromptWorktree(task.rowId, tracked.worktree, ENV_ROOT)
  const display = displayLeaseOf(tracked)
  const ctx = {
    branch: task.branch, worktree, pr: task.pr, type: task.type, base: baseOf(cfg),
    ...(display !== undefined ? { display } : {}),
  }
  const sessionID = await createSession({
    directory: worktree !== "" ? worktree : cfg.local,
    title: `🧪 ${projectName}: тестирование — ${tracked.title || task.title}`,
    agent: TESTER.agent,
    model: TESTER.model,
  })
  if (sessionID === null) throw new Error("тест-сессия не создалась")
  const sent = await promptSession(sessionID, testingSpawnPrompt(task.rowId, projectName, ctx))
  if (!sent) {
    await restCall(`/session/${sessionID}`, { method: "DELETE" }).catch(() => {})
    throw new Error("тест-промпт не ушёл")
  }
  tracked.testSessionID = sessionID
  tracked.testAttempts = 0
  tracked.sessionHistory = [...(tracked.sessionHistory ?? []), sessionID]
  saveState(state)
  await claimSessionCell(board, task.rowId, task.bodyDoc, tracked.title || task.title, { [TEST_SESSION_CELL]: sessionID })
  writeTaskStartSignal(task.rowId, sessionID)
  log(`задача ${task.rowId.slice(0, 8)}: тест-сессия ${sessionID}`)
}

/** Эскалация фейлов спавна тестировщика: 3 подряд → Needs attention. */
async function failTesting(
  task: TaskRef, board: BoardIds, tracked: TrackedTask, state: FactoryState, seed: number, err: unknown,
): Promise<void> {
  const attempts = seed + 1
  tracked.testAttempts = attempts
  tracked.testSessionID = ""
  saveState(state)
  log(`задача ${task.rowId.slice(0, 8)}: тест-старт не удался (попытка ${attempts}) — ${err}`)
  if (attempts >= MAX_ATTEMPTS) {
    await setCells(board, task.rowId, { Status: "Needs attention" })
    // lastStatus — сразу за доской: перестановка статуса юзером посчитается
    // новым входом (seed=0), паритет failReview
    tracked.lastStatus = "Needs attention"
    saveState(state)
    await notify(tracked,
      `[factory] тестировщик не смог завестись (попытка ${attempts}). Проверь лог ${LOG_PATH}; ` +
      "чтобы перезапустить тестирование — переставь статус «Тестирование» (счётчик попыток обнулится).")
  }
}

interface ReviewDeps {
  row: TaskRef
  tracked: TrackedTask
  project: string
  cfg: ProjectCfg
  board: BoardIds
  state: FactoryState
  fresh: TaskComment[]
  consumeComments: () => void
  refreshBodyBaseline: () => Promise<void>
}

/** Исполнение решения routeTask. Возвращает "skip-rest", когда этот тик по
 *  задаче закончен целиком (рестарт девелопера — его запись state заменена). */
async function dispatchReviewAction(
  action: ReturnType<typeof routeTask>, d: ReviewDeps,
): Promise<"skip-rest" | undefined> {
  const shortId = d.row.rowId.slice(0, 8)
  switch (action.t) {
    case "spawn-reviewer": {
      // новый вход в ревью-статус или спавн держателя другого вида ревью
      // (план → результат и обратно; legacy без вида) = свежая попытка
      // (сброс счётчика фейлов): юзер/дев переустановили статус после
      // Needs attention или стадия сменила держателя
      const entering = reviewKindOf(d.tracked.lastStatus ?? "") !== action.kind ||
        d.tracked.reviewKind !== action.kind
      const seed = entering ? 0 : d.tracked.reviewAttempts ?? 0
      try {
        await startReview(d.row, d.project, d.cfg, d.board, d.state, action.kind, d.tracked)
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } catch (e) {
        await failReview(d.row, d.board, d.tracked, d.state, seed, e)
      }
      return undefined
    }
    case "rerun-reviewer": {
      const ok = await promptSession(
        d.tracked.reviewSessionID ?? "",
        reviewRerunPrompt(d.row.rowId, action.kind, {
          branch: d.row.branch,
          worktree: resolvePromptWorktree(d.row.rowId, d.tracked.worktree, ENV_ROOT),
          pr: d.row.pr,
          type: d.row.type,
        }),
      )
      if (ok) {
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: доревью-промпт не ушёл — следующий тик перепроверит сессию`)
      }
      return undefined
    }
    case "comments-to-reviewer": {
      const ok = await promptSession(d.tracked.reviewSessionID ?? "", reviewCommentsPrompt(d.row.rowId, d.fresh))
      if (ok) {
        d.consumeComments()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: комменты ревьюверу не ушли — граница не сдвинута`)
      }
      return undefined
    }
    case "spawn-tester": {
      // новый вход в «Тестирование» = свежая попытка (сброс счётчика фейлов):
      // ревьювер поставил статус заново после Needs attention или баг-петли
      const entering = (d.tracked.lastStatus ?? "") !== TESTING_STATUS
      const seed = entering ? 0 : d.tracked.testAttempts ?? 0
      try {
        await startTesting(d.row, d.project, d.cfg, d.board, d.state, d.tracked)
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } catch (e) {
        await failTesting(d.row, d.board, d.tracked, d.state, seed, e)
      }
      return undefined
    }
    case "rerun-tester": {
      const display = displayLeaseOf(d.tracked)
      const ok = await promptSession(
        d.tracked.testSessionID ?? "",
        testingRerunPrompt(d.row.rowId, {
          branch: d.row.branch,
          worktree: resolvePromptWorktree(d.row.rowId, d.tracked.worktree, ENV_ROOT),
          pr: d.row.pr,
          type: d.row.type,
          ...(display !== undefined ? { display } : {}),
        }),
      )
      if (ok) {
        d.consumeComments()
        await d.refreshBodyBaseline()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: дотест-промпт не ушёл — следующий тик перепроверит сессию`)
      }
      return undefined
    }
    case "comments-to-tester": {
      const ok = await promptSession(d.tracked.testSessionID ?? "", testingCommentsPrompt(d.row.rowId, d.fresh))
      if (ok) {
        d.consumeComments()
        saveState(d.state)
      } else {
        log(`задача ${shortId}: комменты тестировщику не ушли — граница не сдвинута`)
      }
      return undefined
    }
    case "return-to-dev": {
      const devLiveness = d.tracked.sessionID === "" ? "dead" : await sessionLiveness(d.tracked.sessionID)
      if (devLiveness === "unknown") {
        // транзиентная ошибка REST — не плодим дубли сессий, следующий тик перепроверит
        log(`задача ${shortId}: возврат из ревью, живость девелопера неизвестна — пропуск`)
        return undefined
      }
      // конфликтный PR → конфликт-kickoff вместо generic-возврата (и при
      // вердикте ревьювера, и при флипе владельца: «вмешайся и почини»)
      const conflicted = await prConflicted(d.row.pr, d.cfg.local)
      if (devLiveness === "alive") {
        const ok = await promptSession(
          d.tracked.sessionID,
          // живая сессия инит получила при своём спавне — будильник без
          // init (дельта-принцип); init только на свежих сессиях (startTask)
          conflicted
            ? conflictFixPrompt(d.row.rowId, d.row.pr, d.project, { base: baseOf(d.cfg) })
            : reviewReturnPrompt(d.row.rowId),
        )
        if (ok) {
          // один промпт вместо двойной доставки (промпт + wakeup тем же
          // тиком): агент перечитывает task get вместе с комментами/телом
          d.consumeComments()
          await d.refreshBodyBaseline()
          d.tracked.lastDevPromptAt = Date.now()
          saveState(d.state)
        } else {
          log(`задача ${shortId}: возврат из ревью — промпт девелоперу не ушёл`)
        }
        return undefined
      }
      // сессия девелопера потеряна — свежий старт (контекст живёт в задаче),
      // агентские сессии сохраняем: инвариант 1 задача = 1 ревьювер и
      // 1 тестировщик. startTask заменяет запись state — дальше по циклу
      // не идём: свежая сессия уже получила рестарт-промпт (конфликтный или
      // нейтральный)
      const agentSessions = d.tracked
      try {
        await startTask(
          d.row, d.project, d.cfg, d.board, d.state, d.tracked.attempts,
          conflicted ? "conflict-fix" : "continue",
        )
        const reborn = d.state.tasks[d.row.rowId]
        if (reborn !== undefined) {
          preserveAgentSessions(agentSessions, reborn)
          saveState(d.state)
        }
        log(`задача ${shortId}: возврат из ревью при мёртвом девелопере — рестарт сессии${conflicted ? " (конфликт-fix)" : ""}`)
      } catch (e) {
        await registerFailure(d.row, d.project, d.board, d.state, d.tracked.attempts, e)
      }
      return "skip-rest"
    }
    case "noop":
      return undefined
  }
}

export async function tickProject(state: FactoryState, projectName: string, cfg: ProjectCfg): Promise<void> {
  const board = await findBoard(cfg.board)
  // per-tick кэш экспортов тел: комменты, bodyEdit-гейт и comment-ветка
  // читают один и тот же док одним физическим вызовом ядра (Low-1)
  const bodyExports = new Map<string, string | null>()
  // known bodyDocs из state: detached-строки с излеченным треком идут мимо
  // фоллбека (без API-вызовов); читается до волны спавнов — новые старты
  // этого тика резолвятся фоллбеком как раньше
  const knownBodyDocs = new Map(Object.entries(state.tasks).map(([id, t]) => [id, t.bodyDoc]))
  const rows = await readTasks(board, knownBodyDocs)
  // наблюдаемость полноты скана: N сверяемо со счётчиком ядра (view.rowCount)
  log(`проход ${projectName}: строк ${rows.length}`)
  const counters: PassCounters = {
    devSpawned: 0, reviewSpawned: 0, testerSpawned: 0, wakeups: 0, cleanups: 0,
    envCreated: 0, envRemoved: 0, envAtticked: 0,
    watched: 0, failures: [],
    autodoneMerged: 0, autodoneBounced: 0,
  }

  // 1. новые задачи: Design → девелопер (утряска), In Progress без state →
  //    девелопер (fast-track владельца: сразу реализация), ревью-статус → ревьювер,
  //    «Тестирование» → тестировщик (Draft = черновик триажа, пустой = легаси —
  //    оба не спавнят)
  const started = new Set<string>()
  const spawnable = rows.filter(
    (r) => r.status === "Design" || r.status === "In Progress" ||
      reviewKindOf(r.status) !== null || r.status === TESTING_STATUS)
  const untracked = untrackedTasks(spawnable, state)

  // ——— гард каскада: сверка state с доской до спавна (инцидент 06.11) ———
  // Сирота-спавн: свидетельство спавна в строке без трека в state. Вердикт
  // по каждой untracked строке — untrackedRowDecision (единственная точка):
  // живая дев-заявка (task claim / фабричный спавн с потерей state) — внешняя
  // работа идёт: не спавнить дубль и не считать сиротой; мёртвая — сирота как
  // прежде и остаётся в дев-волне (ре-спавн при allow — автокомпенсация);
  // ревью-свидетельство без трека — сирота как прежде. Спавнить сирот сверх
  // lossMax = гарантированный каскад re-spawn и параллельные сессии.
  // Liveness-пробы параллельны: сериальные await в цикле растягивали тик
  // при волне заявленных строк (культура дев-волны 05.11).
  const externalSkip = new Set<string>()
  const verdicts = await Promise.all(untracked.map(async (r) => {
    const devCell = r.devSession ?? ""
    const liveness = devCell === "" ? "dead" : await sessionLiveness(devCell)
    return { rowId: r.rowId, liveness, decision: untrackedRowDecision(r, liveness) }
  }))
  let orphaned = 0
  for (const { rowId, liveness, decision } of verdicts) {
    if (!decision.devWave) {
      externalSkip.add(rowId)
      log(`внешняя заявка ${rowId.slice(0, 8)}: сессия ${liveness} — спавн не требуется`)
    }
    if (decision.orphan) orphaned++
  }
  const guard = spawnGuardVerdict(
    { orphaned, totalUntracked: untracked.length, holdSignaled: state.holdSignaled, now: Date.now() },
    SPAWN_LIMITS,
  )
  if (guard.t === "hold") {
    await holdSpawns(guard, untracked, orphaned, state, counters)
  } else if (state.holdSignaled !== undefined) {
    // сигнатура ушла (state восстановлен) — эпизод завершён, снимаем флаг:
    // следующий эпизод снова получит alert=first с алертом и бэкапом
    delete state.holdSignaled
    saveState(state)
    log("гард спавна: сигнатура ушла — эпизод HOLD завершён, флаг снят")
  }

  // дев-волна — пачкой параллельно: все ненатрекенные задачи (Design-утряска
  // и fast-track In Progress владельца) получают сессии за ОДИН проход
  // (прод-кейс 05.11: последовательный спавн и слепота страницы 1
  // растягивали волну из 4 задач на 12+ минут). HOLD гарда глушит волну.
  // Строки с живой внешней заявкой (externalSkip) в волну не попадают.
  // Очередь CUP (задача 20261112160316): кап активных (LLM-тратящих) задач
  // глобально по всем проектам; берём первые budget в порядке приоритет →
  // FIFO (rowId = timestamp создания). В пик считаются только high —
  // замороженные не-high слотов не держат. Возвраты трекнутых задач девелоперу
  // и карантинные спавны ревью/теста не гейтятся (продолжение пайпа, не новая
  // работа) — но учитываются в счёте наравне.
  const peakNow = isPeakHours(new Date(), PEAK)
  const devCandidates = guard.t === "allow"
    ? untracked.filter((t) => reviewKindOf(t.status) === null && t.status !== TESTING_STATUS && !externalSkip.has(t.rowId))
    : []
  // пик-заморозка не-high: спавн откладывается до конца окна (выполняющиеся
  // ходы сессий доигрывают — процессы не трогаем)
  const candidatesNow = peakNow
    ? devCandidates.filter((t) => (t.priority ?? "").trim().toLowerCase() === "high")
    : devCandidates
  const activeEntries = Object.values(state.tasks).map((t) => ({
    status: t.lastStatus ?? "",
    priority: t.priority ?? "",
  }))
  const active = activeTaskCountEx(activeEntries, { peak: peakNow })
  const budget = cupBudget(active, CUP)
  const devWave = queueOrder(candidatesNow).slice(0, budget)
  const queued = candidatesNow.length - devWave.length
  if (queued > 0) {
    const head = queueOrder(candidatesNow)[budget]?.title
    log(`очередь CUP: активных=${active}/${CUP}${peakNow ? " (пик: счёт только high)" : ""}, ждут=${queued}, голова — «${head ?? "?"}»`)
  }
  if (peakNow && devCandidates.length > candidatesNow.length) {
    const frozen = devCandidates.length - candidatesNow.length
    log(`часы пик: спавн не-high отложен (${frozen} задач(и), приоритеты medium/low/пусто — до конца окна)`)
  }
  if (devWave.length > 0 && !drain.isStopping()) {
    await runBatch(devWave, async (task) => {
      if (drain.isStopping()) return // стоп в середине пачки — доедет следующий старт
      try {
        await startTask(task, projectName, cfg, board, state, 0, kickoffModeOf(task.status), counters)
        // инвариант против drain-отмены внутри startTask (молчаливый return):
        // сессия есть ⇔ в state появилась запись — иначе спавном не считаем
        if (state.tasks[task.rowId] === undefined) return
        started.add(task.rowId)
        counters.devSpawned++
      } catch (e) {
        await registerFailure(task, projectName, board, state, 0, e)
        counters.failures.push(`${task.rowId.slice(0, 8)}: ${errText(e)}`)
      }
    }, SPAWN_WAVE_CONCURRENCY)
  }

  // карантинные спавны untracked (ревьювер/тестировщик): тоже под гардом
  // (HOLD глушит — каскад агентских сессий так же опасен, как дев-волна)
  const agentUntracked = guard.t === "allow"
    ? untracked.filter((t) => reviewKindOf(t.status) !== null || t.status === TESTING_STATUS)
    : []
  for (const task of agentUntracked) {
    const kind = reviewKindOf(task.status)
    // ревью-статус/«Тестирование» без state (даунтайм демона при переходе) —
    // карантинная запись (девелопер неизвестен) + спавн держателя сессии;
    // lastStatus = текущий, чтобы следующий тик не принял спавн за новый вход
    // (паразитный rerun)
    const quarantineExports = new Map<string, string | null>()
    const last = latestComment(
      (await taskComments(task.bodyDoc, quarantineExports)).filter((c) => isUserComment(c, BOT_USER_ID)),
    )
    const entry: TrackedTask = {
      sessionID: "", worktree: "", branch: "",
      bodyDoc: task.bodyDoc, title: task.title, project: projectName,
      lastCommentAt: last?.createdAt ?? 0, lastCommentId: last?.id ?? "",
      createdAt: Date.now(), attempts: 0, reviewAttempts: 0, testAttempts: 0,
      lastStatus: task.status,
      priority: task.priority,
      bodyUpdated: (await bodyUpdatedAt(task.bodyDoc)) ?? undefined,
      bodyHash: (await bodyHashOf(task.bodyDoc, quarantineExports)) ?? undefined,
    }
    state.tasks[task.rowId] = entry
    try {
      if (kind !== null) {
        await startReview(task, projectName, cfg, board, state, kind, entry)
        counters.reviewSpawned++
      } else {
        await startTesting(task, projectName, cfg, board, state, entry)
        counters.testerSpawned++
      }
    } catch (e) {
      if (kind !== null) await failReview(task, board, entry, state, 0, e)
      else await failTesting(task, board, entry, state, 0, e)
      counters.failures.push(`${task.rowId.slice(0, 8)}: ${errText(e)}`)
    }
    started.add(task.rowId)
  }

  // автодон-строки в Review без state (даунтайм демона на входе в Review):
  // entry + проба этим же тиком основного цикла (НЕ started — строка должна
  // дойти до автодон-хука). Спавна нет — guard каскада не задействован.
  for (const task of rows) {
    if (task.status !== USER_REVIEW_STATUS || !task.autodone) continue
    if (state.tasks[task.rowId] !== undefined || started.has(task.rowId)) continue
    const adExports = new Map<string, string | null>()
    const last = latestComment(
      (await taskComments(task.bodyDoc, adExports)).filter((c) => isUserComment(c, BOT_USER_ID)),
    )
    const entry: TrackedTask = {
      sessionID: "", worktree: "", branch: "",
      bodyDoc: task.bodyDoc, title: task.title, project: projectName,
      lastCommentAt: last?.createdAt ?? 0, lastCommentId: last?.id ?? "",
      createdAt: Date.now(), attempts: 0, reviewAttempts: 0, testAttempts: 0,
      lastStatus: task.status,
      priority: task.priority,
      bodyUpdated: (await bodyUpdatedAt(task.bodyDoc)) ?? undefined,
      bodyHash: (await bodyHashOf(task.bodyDoc, adExports)) ?? undefined,
    }
    state.tasks[task.rowId] = entry
    saveState(state)
    log(`задача ${task.rowId.slice(0, 8)}: autodone в Review без state — карантинная запись + проба`)
  }

  // 2. активные этой доски (state общий на все проекты)
  for (const rowId of Object.keys(state.tasks)) {
    const tracked = state.tasks[rowId]
    if ((tracked.project || "uwuwu") !== projectName) continue
    if (started.has(rowId)) continue // стартован этим тиком — rows устарели
    const row = rows.find((r) => r.rowId === rowId)
    // suspect-механика: необратимую чистку (finishTask = delete state +
    // DELETE ревью-сессии) страхуем от одиночного промаха скана — строка
    // должна отсутствовать в двух проходах подряд
    const missing = rowMissingAction(tracked.missingSince, row !== undefined)
    if (missing === "clear") {
      delete tracked.missingSince
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: строка вернулась в скан — suspect-пометка снята`)
    } else if (missing === "mark") {
      tracked.missingSince = new Date().toISOString()
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: строки нет в скане — suspect, чистка не раньше следующего прохода`)
    } else if (missing === "clean") {
      log(`задача ${rowId.slice(0, 8)}: строки нет в скане 2 прохода подряд (с ${tracked.missingSince}) — чистка`)
      await finishTask(rowId, tracked, cfg, state, counters)
      counters.cleanups++
      continue
    }
    if (row === undefined) continue // mark: маршрутизировать нечего

    // ——— самоисцеление: adopt + базлайн тела (до маршрутизации/комментов) ———
    // привязка/фоллбек в state пусты → принять из свежего скана; без засева
    // bodyUpdated канал body-edit-wakeup (ниже требует непустого базлайна)
    // оставался бы мёртвым у исцелённых треков
    const adopted = adoptBodyDoc(tracked, row)
    let healed = false
    if (adopted !== null) {
      tracked.bodyDoc = adopted
      healed = true
      log(`задача ${rowId.slice(0, 8)}: пустой bodyDoc трека — принята привязка строки ${adopted.slice(0, 8)}`)
    }
    // приоритет строки — свежий из скана (счёт CUP в пик смотрит сюда)
    if ((tracked.priority ?? "") !== (row.priority ?? "")) {
      tracked.priority = row.priority
      healed = true
    }
    // галочка снята → сброс seen: переустановка позже = свежая активация
    // автодона (ревью Common-2)
    if (!(row.autodone ?? false) && tracked.autodoneSeen === true) {
      tracked.autodoneSeen = false
      healed = true
    }
    if (tracked.bodyDoc !== "" && (tracked.bodyUpdated ?? "") === "") {
      const seeded = await bodyUpdatedAt(tracked.bodyDoc)
      if (seeded !== null) {
        tracked.bodyUpdated = seeded
        healed = true
      }
    }
    // bodyHash здесь НЕ сеется: экспорт тяжёлый (интеграционный тест
    // done-clean падал на таймауте мока), первая lastmod-дельта даст
    // честную оценку без волны ложных будок
    if (healed) saveState(state)

    // Done (мерж-крон агента, Spike-агент, руки юзера) → чистка ворктри
    if (row.status === "Done") {
      log(`задача ${rowId.slice(0, 8)}: Done (закрыта агентом/юзером) — чистка`)
      // инвариант гарда каскада: строка Done без сессионных ячеек —
      // переоткрытие не выглядит сиротой-спавном. Только путь Done (строка
      // наблюдается в скане): suspect-clean пишет ячейки в пустоту —
      // setCellsSafe дал бы шумный catch-лог на каждой чистке
      await setCellsSafe(board, rowId, { [DEV_SESSION_CELL]: "", [REVIEW_SESSION_CELL]: "", [TEST_SESSION_CELL]: "" })
      await finishTask(rowId, tracked, cfg, state, counters)
      counters.cleanups++
      continue
    }

    // ——— маршрутизация: решение ДО перезаписи lastStatus и карантинных веток ———
    const comments = await taskComments(tracked.bodyDoc, bodyExports)
    const users = comments.filter((c) => isUserComment(c, BOT_USER_ID))
    // легаси-переход (деплой фильтра self-эха): старый id при нулевом времени —
    // фоллбек по времени отдал бы всю историю чужих AGENTS-комментов одной волной
    if (legacyRebaselineNeeded({ at: tracked.lastCommentAt, id: tracked.lastCommentId }, users)) {
      const last = latestComment(users)
      tracked.lastCommentAt = last?.createdAt ?? 0
      tracked.lastCommentId = last?.id ?? ""
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: легаси-граница комментов — re-baseline на текущий срез без будки`)
    }
    const fresh = newUserComments(comments, { at: tracked.lastCommentAt, id: tracked.lastCommentId }, BOT_USER_ID)
    // сесс-скопед срез self-эха: активность адресата по статусу (в ревью —
    // ревьювер, в «Тестировании» — тестировщик, иначе девелопер); сбой
    // активности — без среза
    const slot = sessionSlotOf(row.status)
    const addresseeSession = slot === "review"
      ? tracked.reviewSessionID ?? ""
      : slot === "test"
        ? tracked.testSessionID ?? ""
        : tracked.sessionID
    const activity = addresseeSession !== "" ? await lastSessionActivity(addresseeSession) : null
    const { foreign, ownCount } = freshForeignComments(fresh, activity === "invalid" ? null : activity)
    if (ownCount > 0) {
      log(`задача ${rowId.slice(0, 8)}: self-эхо срезано (${ownCount}/${fresh.length} комментов)`)
    }
    const reviewerLiveness = tracked.reviewSessionID === undefined || tracked.reviewSessionID === ""
      ? "dead"
      : await sessionLiveness(tracked.reviewSessionID)
    const testerLiveness = tracked.testSessionID === undefined || tracked.testSessionID === ""
      ? "dead"
      : await sessionLiveness(tracked.testSessionID)
    let action = routeTask({
      status: row.status,
      lastStatus: tracked.lastStatus ?? "",
      reviewerAlive: reviewerLiveness === "alive" ? true : reviewerLiveness === "dead" ? false : null,
      reviewKind: tracked.reviewKind,
      testerAlive: testerLiveness === "alive" ? true : testerLiveness === "dead" ? false : null,
      freshComments: foreign,
    })
    // пик-заморозка не-high: спавны/будильники ревью-контура откладываются
    // до конца окна (решение в лог — «почему ветка не взята», статусные
    // записи и сталл-эскалации ниже идут как обычно — токен не тратят)
    const rowPriority = (row.priority ?? "").trim().toLowerCase()
    if (peakNow && rowPriority !== "high" && action.t !== "noop") {
      log(`часы пик: действие «${action.t}» по не-high задаче ${rowId.slice(0, 8)} отложено до конца окна`)
      action = { t: "noop" }
    }
    let commentsConsumed = false
    const consumeComments = (): void => {
      const lastComment = fresh[fresh.length - 1]
      if (lastComment === undefined) return
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      commentsConsumed = true
    }
    /** базлайн тела: дев правил тело перед передачей мяча — этим не будить
     *  (lastmod + хэш контента вне «Комменты»: собственные записи отчёта
     *  меняют контент — без обновления хэша след lastmod-only дал бы будку) */
    const refreshBodyBaseline = async (): Promise<void> => {
      const updated = await bodyUpdatedAt(tracked.bodyDoc)
      if (updated !== null) tracked.bodyUpdated = updated
      const hash = await bodyHashOf(tracked.bodyDoc, bodyExports)
      if (hash !== null) tracked.bodyHash = hash
    }

    const skipRest = await dispatchReviewAction(action, {
      row, tracked, project: tracked.project || projectName, cfg, board, state, fresh: foreign,
      consumeComments, refreshBodyBaseline,
    })
    if (action.t !== "noop") counters.wakeups++ // спавн/реран/комменты/возврат — будильники ревью-контура
    if (skipRest === "skip-rest") continue

    // ——— автодон: галочка владельца на входе в Review (задача 20261112160316) ———
    // Решение до перезаписи lastStatus: bounce оставляет lastStatus в стадии
    // пайпа — следующий тик возвращает мяч девелоперу штатным return-to-dev.
    // Эпизод: pending бежит; терминальный исход (merged/escalated) того же
    // эпизода (lastStatus всё ещё Review) не перезапускается — но НОВЫЙ вход
    // в Review (lastStatusedge) или переустановка галочки (off→on, ревью
    // Common-2) перезапускают эпизод: эскалация не убивает автодон навсегда.
    const autodoneFreshActivation = (row.autodone ?? false) && tracked.autodoneSeen !== true
    const autodoneNewEntry = tracked.lastStatus !== USER_REVIEW_STATUS
    if (row.status === USER_REVIEW_STATUS && (row.autodone ?? false) &&
        (tracked.autodoneState === undefined || tracked.autodoneState === "pending" ||
         autodoneNewEntry || autodoneFreshActivation)) {
      tracked.autodoneSeen = true
      await runAutodoneTick(row, tracked, { cfg, board, state, counters })
      continue
    }

    // запоминаем наблюдённый статус — граница перехода для след. тика
    if (tracked.lastStatus !== row.status) {
      tracked.lastStatus = row.status
      // смена статуса = новый эпизод застоя: пере-вход в In Progress должен
      // уметь сигналить снова (наш собственный сигнал уводит строку из
      // In Progress — без сброса флаг заглушил бы повторные застои навсегда)
      if (tracked.stallSignaled === true) {
        delete tracked.stallSignaled
        log(`задача ${rowId.slice(0, 8)}: статус сменился — флаг застоя сброшен (новый эпизод)`)
      }
      if (tracked.reviewStallSignaled === true) {
        delete tracked.reviewStallSignaled
        log(`задача ${rowId.slice(0, 8)}: статус сменился — флаг ревью-тишины сброшен (новый эпизод)`)
      }
      saveState(state)
    }

    // ——— гвард тишины агентской стадии (инцидент 07.11, задача 20261107142321):
    // ход держателя сессии (ревьювер/тестировщик) завершился, статус не ушёл
    // из стадии — routeTask в этой геометрии вечный noop (живая сессия,
    // нет свежих комментов). Тишина дольше порога → сигнал юзеру +
    // Needs attention, раз за эпизод (флаг reviewStallSignaled; сброс —
    // смена статуса выше или вернувшаяся активность держателя).
    // Восстановление — штатные пути: перестановка статуса будит ту же сессию
    // дельта-промптом (rerun).
    // Только noop: spawn/rerun/comments только что побудили держателя —
    // его активность ещё не отражает ход, детекция была бы ложной.
    // Активность уже снята выше (addresseeSession в агентском статусе — та же
    // сессия; до этой ветки noop её не трогает) — не дублируем вызов.
    const holderLiveness = slot === "review" ? reviewerLiveness : testerLiveness
    if (action.t === "noop" && slot !== "dev" && holderLiveness === "alive") {
      if (activity === "invalid") {
        log(`задача ${rowId.slice(0, 8)}: активность сессии держателя неизвестна — пропуск тика`)
      } else {
        const verdict = reviewStallVerdict({
          status: row.status,
          holderAlive: holderLiveness === "alive",
          lastActivityAt: activity,
          signaled: tracked.reviewStallSignaled === true,
          now: Date.now(),
        }, STALL_MS)
        if (verdict.t === "signal") {
          // эскалация как failReview/stall: коммент + Needs attention;
          // lastStatus сразу за доской — перестановка юзером статуса стадии
          // посчитается новым входом (rerun). continue: «пайп
          // девелопера» ниже может догнать свои уведомления (мёртвая дев-
          // сессия) — двойной сигнал в один тик юзеру не нужен
          const quietDiag = activity === null ? "сообщений в сессии нет" : `тишина > ${STALL_MIN} мин`
          log(`задача ${rowId.slice(0, 8)}: агентский статус при живой сессии держателя (${quietDiag}) — сигнал + Needs attention`)
          if (DRY_RUN) {
            // setCells/notify в DRY_RUN сами глухие, а state-мутации не пишем:
            // без флага анти-спам в песочнице не работает — сигнал повторялся
            // бы каждые 2 тика (паритет дев-сталла, расхождение задокументировано логом)
            log(`DRY_RUN: ревью-эскалация Needs attention + коммент — пропущены`)
          } else {
            await setCells(board, rowId, { Status: "Needs attention" })
            tracked.reviewStallSignaled = true
            tracked.lastStatus = "Needs attention"
            saveState(state)
            await notify(tracked, verdict.text)
          }
          continue
        } else if (verdict.t === "none" && tracked.reviewStallSignaled === true) {
          delete tracked.reviewStallSignaled
          saveState(state)
          log(`задача ${rowId.slice(0, 8)}: эпизод тишины агентской стадии завершён — флаг снят (активность держателя вернулась)`)
        }
      }
    }

    // ——— пайп девелопера: комменты/тело/живость (комменты мог съесть ревьювер) ———
    const devFresh = commentsConsumed ? [] : foreign

    // будильник правок ТЕЛА без комментов — до карантинных веток: адресат
    // по статусу (в агентской стадии — держатель сессии, даже если девелопер
    // в карантине). Гейт: lastmod двигают и UI-пересохранения, и собственные
    // записи агента — будим только на контентную дельту (хэш норм-формы вне
    // «Комменты») не из хода адресата (дефекты №3/№4/№5)
    if (devFresh.length === 0) {
      const updated = await bodyUpdatedAt(tracked.bodyDoc)
      const lastmodChanged = updated !== null
        && (tracked.bodyUpdated ?? "") !== ""
        && updated > (tracked.bodyUpdated ?? "")
      if (lastmodChanged) {
        const route = bodyEditRoute({
          status: row.status,
          reviewSessionID: tracked.reviewSessionID ?? "",
          testSessionID: tracked.testSessionID ?? "",
          devSessionID: tracked.sessionID,
        })
        if (route.t === "wake") {
          const md = await bodyExport(tracked.bodyDoc, bodyExports)
          // сбой экспорта → деградация к будке (тихий скип потерял бы правку)
          const hash = md === null ? null : bodyContentHash(md)
          const lastmodMs = md === null ? null : bodyLastmodMs(md)
          const activity = await lastSessionActivity(route.target)
          const ownEdit = lastmodMs !== null && typeof activity === "number" && lastmodMs <= activity
          const contentChanged = hash === null || hash !== (tracked.bodyHash ?? "")
          if (!contentChanged) {
            tracked.bodyUpdated = updated
            if (hash !== null) tracked.bodyHash = hash
            saveState(state)
            log(`задача ${rowId.slice(0, 8)}: lastmod-only правка тела (UI-пересохранение) — без будки`)
          } else if (ownEdit) {
            tracked.bodyUpdated = updated
            if (hash !== null) tracked.bodyHash = hash
            saveState(state)
            log(`задача ${rowId.slice(0, 8)}: правка тела из хода адресата — без будки`)
          } else {
            const bodyKey = `body:${hash ?? updated}`
            if (wakeupDedup(tracked.lastWakeup, bodyKey, Date.now(), WAKEUP_DEDUP_MS) === "skip") {
              tracked.bodyUpdated = updated
              if (hash !== null) tracked.bodyHash = hash
              saveState(state)
              log(`задача ${rowId.slice(0, 8)}: дубль bodyEdit-wakeup в окне дедупа — скип`)
            } else {
              log(`задача ${rowId.slice(0, 8)}: контентная правка тела → wakeup (${route.role})`)
              if (await promptSession(route.target, bodyEditPrompt(rowId))) {
                tracked.bodyUpdated = updated
                if (hash !== null) tracked.bodyHash = hash
                tracked.lastWakeup = { key: bodyKey, at: Date.now() }
                if (route.role === "девелопер") tracked.lastDevPromptAt = Date.now()
                saveState(state)
                counters.wakeups++
              }
            }
          }
          continue
        }
        if (route.updateBaseline) {
          // гейт статуса (Review = мяч у владельца): правка проглочена осознанно —
          // базлайн двигаем, иначе каждые 3 мин перевычисление и лог-шум
          tracked.bodyUpdated = updated
          const md = await bodyExport(tracked.bodyDoc, bodyExports)
          const hash = md === null ? null : bodyContentHash(md)
          if (hash !== null) tracked.bodyHash = hash
          saveState(state)
          log(`задача ${rowId.slice(0, 8)}: правка тела на Review (мяч у владельца) — без будки`)
          continue
        }
        // таргета нет (дев в карантине) — базлайн не двигаем:
        // добудим адресата на следующем тике
      }
    }

    if (tracked.sessionID === "") {
      // карантин: ждём явной активности юзера
      if (devFresh.length === 0) continue
      await restartTask(row, tracked.project || projectName, cfg, board, state, devFresh)
      continue
    }

    if (devFresh.length === 0) {
      // юзер молчит, но сессия могла умереть посреди прогона
      const quiet = await sessionLiveness(tracked.sessionID)
      if (quiet === "dead") {
        log(`задача ${rowId.slice(0, 8)}: сессия умерла (юзер молчит) — карантин + уведомление`)
        tracked.sessionID = ""
        saveState(state)
        await notify(tracked,
          `[factory] сессия разработки потеряна. Напиши комментарий — перезапущу (несохранённое в ворктри будет перезаписано с ${baseOf(cfg)}).`)
      } else if (quiet === "alive" && row.status === "In Progress") {
        // сессия жива — ловим зависший ход: агент молчит дольше порога
        // (активный ход рождает сообщения на каждом шаге LLM). Неизвестная
        // активность (REST/формат) — пропуск тика без операций с флагом
        const activity = await lastSessionActivity(tracked.sessionID)
        if (activity === "invalid") {
          log(`задача ${rowId.slice(0, 8)}: активность сессии неизвестна — пропуск тика`)
        } else {
          // Единые часы сталла (stallClock): вердикт и pre-чек ленивых проб
          // считают тишину от одной точки — последний сигнал сессии. Свежий
          // будильник сбрасывает часы (инцидент 09.11: reviewReturnPrompt
          // и сталл-кик из одной геометрии «тишина от активности» — два
          // противоречащих сигнала одним тиком)
          const clock = stallClock(
            { lastActivityAt: activity, lastDevPromptAt: tracked.lastDevPromptAt ?? null },
            STALL_MS,
            Date.now(),
          )
          if (!clock.stalled && clock.fromWake && activity !== null
              && Date.now() - activity > STALL_MS) {
            log(`задача ${rowId.slice(0, 8)}: будильник сессии свежий — часы от wake, тишина ${clock.quietMin} мин, детекция ждёт ответ`)
          }
          const { stalled } = clock
          // кэш мержа действителен, пока совпадает с PR-ячейкой или ячейка
          // пуста (позитив фоллбек-пробы по ветке); смена PR на заполненной
          // клетке — stacked/ремедиация, штатный пайп-путь — инвалидирует
          let prMerged = (tracked.mergedPrUrl ?? "") !== ""
            && (row.pr === "" || tracked.mergedPrUrl === row.pr)
          // gh-проба ленивая (вердикт может стать signal) и только до первого
          // MERGED: merge необратим — без кэша это была бы проба каждые 3 мин
          // на всё время ожидания Done (дни). signaled=true глушит вердикт —
          // проба не нужна. Пустая клетка при непустой ветке — фоллбек по
          // ветке (инцидент 09.11: мерженный PR не защитил от кика)
          if (stalled && !prMerged && tracked.stallSignaled !== true) {
            const probe = row.pr !== ""
              ? await prProbeByCell(row.pr)
              : row.branch !== "" ? await prProbeByBranch(cfg.repo, row.branch) : null
            if (probe === null) {
              // нет ни PR-клетки, ни ветки — пробовать нечего
              tracked.prProbeUnknownStreak = undefined
              saveState(state)
            } else {
              const pv = prProbeVerdict(probe.result, tracked.prProbeUnknownStreak ?? 0)
              tracked.prProbeUnknownStreak = pv.streakAfter === 0 ? undefined : pv.streakAfter
              if (pv.degraded) {
                log(`задача ${rowId.slice(0, 8)}: gh не отвечает ${pv.streakAfter} тиков подряд — сталл-детект деградировал к not-merged (страховка ADR-044)`)
              }
              if (pv.merged && probe.mergedUrl !== null) {
                tracked.mergedPrUrl = probe.mergedUrl
                prMerged = true
                log(`задача ${rowId.slice(0, 8)}: PR смержен — тишина сессии = ожидание приёмки (Done владельца), stall-сигнал не нужен`)
              }
              saveState(state)
              if (pv.defer) {
                // unknown до предела: сбой gh — не повод для кика, вердикт в
                // этом тике не строится (первый же unknown иначе кикал бы
                // задачу со смерженным PR — вред, ради которого существует
                // проба); полоса дольше предела деградирует выше
                log(`задача ${rowId.slice(0, 8)}: merged-проба не удалась (gh), полоса ${pv.streakAfter}/${PR_PROBE_UNKNOWN_MAX} — кик отложен на тик`)
                continue
              }
            }
          }
          // при валидном кэше вердикт всё равно none — факты для текста
          // сигнала не строятся; ворктри из state — для env-задач он всегда
          // записан фабрикой, env:false живёт вне механики
          const branchInWorktree = !prMerged && stalled && tracked.worktree !== ""
          const verdict = stallVerdict({
            status: row.status,
            sessionAlive: true,
            lastActivityAt: activity,
            lastDevPromptAt: tracked.lastDevPromptAt ?? null,
            branch: row.branch,
            branchInWorktree,
            prMerged,
            signaled: tracked.stallSignaled === true,
            now: Date.now(),
          }, STALL_MS)
          if (verdict.t === "signal") {
            // эскалация как у registerFailure/failReview: коммент + Needs
            // attention; lastStatus сразу за доской (как в failReview)
            log(`задача ${rowId.slice(0, 8)}: In Progress при живой сессии, тишина > ${STALL_MIN} мин — сигнал + Needs attention`)
            if (DRY_RUN) {
              // setCells/notify в DRY_RUN сами глухие, а state-мутации не пишем:
              // без флага анти-спам в песочнице не работает — сигнал повторялся
              // бы каждые 2 тика (расхождение с продом задокументировано логом)
              log(`DRY_RUN: эскалация Needs attention + коммент — пропущены`)
            } else {
              await setCells(board, rowId, { Status: "Needs attention" })
              tracked.stallSignaled = true
              tracked.lastStatus = "Needs attention"
              saveState(state)
              await notify(tracked, verdict.text)
            }
          } else if (verdict.t === "none" && tracked.stallSignaled === true) {
            delete tracked.stallSignaled
            saveState(state)
            // до этой ветки штатный путь «возврат в In Progress» не доходит —
            // флаг снимает сброс на смене статуса выше; здесь — эпизод,
            // завершённый без смены статуса (наш сигнал поставил Needs attention,
            // rollback-окно старого кода), формулировка обобщённая
            log(`задача ${rowId.slice(0, 8)}: эпизод застоя завершён — флаг снят (статус вне In Progress или активность вернулась)`)
          }
        }
      }
      continue
    }

    const liveness = await sessionLiveness(tracked.sessionID)
    if (liveness === "unknown") {
      log(`задача ${rowId.slice(0, 8)}: живость сессии неизвестна — пропускаю тик`)
      continue
    }
    if (liveness === "dead") {
      log(`задача ${rowId.slice(0, 8)}: сессия умерла — перезапуск`)
      await restartTask(row, tracked.project || projectName, cfg, board, state, devFresh)
      continue
    }

    // правка ТЕЛА при наличии комментов: wakeup-комменты ниже разбудят той
    // же пачкой — тело агент перечитает сам; хэш пересчитывается (юзерская
    // правка тела могла прийти вместе с комментом), lastmod двигается
    const updatedWithComments = await bodyUpdatedAt(tracked.bodyDoc)
    if (
      updatedWithComments !== null &&
      (tracked.bodyUpdated ?? "") !== "" &&
      updatedWithComments > (tracked.bodyUpdated ?? "")
    ) {
      const hashWithComments = await bodyHashOf(tracked.bodyDoc, bodyExports)
      if (hashWithComments !== null) tracked.bodyHash = hashWithComments
      tracked.bodyUpdated = updatedWithComments
      saveState(state)
    }

    if (devFresh.length === 0) continue
    const wakeupKey = `comment:${devFresh.map((x) => x.id).join(",")}`
    if (wakeupDedup(tracked.lastWakeup, wakeupKey, Date.now(), WAKEUP_DEDUP_MS) === "skip") {
      // та же дельта уже доставлена — границу съедаем без повторной будки (№5.2)
      const lastComment = devFresh[devFresh.length - 1]
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      saveState(state)
      log(`задача ${rowId.slice(0, 8)}: дубль comment-wakeup в окне дедупа — скип`)
      continue
    }
    log(`задача ${rowId.slice(0, 8)}: ${devFresh.length} новых коммент(ов) → сессия`)
    if (await promptSession(tracked.sessionID, wakeupPrompt(rowId, devFresh))) {
      const lastComment = devFresh[devFresh.length - 1]
      tracked.lastCommentAt = lastComment.createdAt
      tracked.lastCommentId = lastComment.id
      tracked.agentActive = true
      tracked.lastWakeup = { key: wakeupKey, at: Date.now() }
      tracked.lastDevPromptAt = Date.now()
      saveState(state)
      counters.wakeups++
    } else {
      log(`задача ${rowId.slice(0, 8)}: промпт не ушёл — граница комментов не сдвинута`)
    }
  }

  // сводка прохода: «молчаливые тики» видны — что сделано и сколько живых
  counters.watched = Object.values(state.tasks)
    .filter((t) => (t.project || "uwuwu") === projectName).length
  log(formatPassSummary(projectName, counters))
}

let tickCount = 0

async function main(): Promise<void> {
  // env шлюза обязателен на старте (раньше это делал process.exit на импорте
  // mcp.ts — теперь проверка явная, модуль шарится с ленивой конфигурацией)
  if (!mcp.mcpConfigured()) {
    console.error("hub: нужен UWUWU_HUB_REPO")
    process.exit(1)
  }
  // env-оверрайд проектов: валидируем ДО лока — невалидный конфиг не должен
  // даже притворяться демоном (тестовые доски для ONCE-прогонов)
  let projects: Record<string, ProjectCfg>
  try {
    projects = parseProjects(process.env.UWUWU_FACTORY_PROJECTS_JSON, DEFAULT_PROJECTS)
  } catch (e) {
    console.error(errText(e))
    process.exit(1)
  }
  // гард dry-run ДО лока: невалидный режим не должен даже притворяться
  // демоном (фиктивные записи не должны попадать в прод-state). Эффективная
  // песочница — с авто-изоляцией ветки: там прод-state и так недостижим.
  if (DRY_RUN && !STATE_RESOLVED.sandboxed) {
    console.error("DRY_RUN требует UWUWU_FACTORY_STATE_DIR (песочница state) — фиктивные записи не должны попадать в прод-state")
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: drain.stop })) {
    console.error("другой живой инстанс uwuwu-factory держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  rotateLog()
  await mcp.connect()
  // пиннинг за нодами (ADR-076): док-истина топологии; перепин или незапиненный
  // конфиг в проде — громкий отказ старта; песочница смоук-тиков (ADR-059)
  // unknown-проекты пропускает (их в доке нет и не должно быть)
  const pinning = await checkNodePinning(mcp.tool, {
    factoryProjects: Object.keys(projects),
    sandboxed: STATE_RESOLVED.sandboxed,
    envNodeName: process.env.UWUWU_NODE_NAME,
    hostname: hostname(),
  })
  failPinnedStart(pinning)
  log(`пиннинг: нода ${pinning.nodeName} подтверждена${pinning.sandboxSkipped.length > 0 ? ` (песочница пропустила незапиненные: ${pinning.sandboxSkipped.join(", ")})` : ""}`)
  log(`uwuwu-factory: старт (проекты: ${Object.keys(projects).join(", ")}, poll=${POLL_SEC}s, stall=${STALL_MIN}m, model=${MODEL.id}, гард каскада: loss>${SPAWN_LIMITS.lossMax}/burst>${SPAWN_LIMITS.burstMax}, CUP=${CUP}, пик=${PEAK.enabled ? `${PEAK.startMin}-${PEAK.endMin} UTC` : "off"}, autodone-timeout=${Math.round(AUTODONE_TIMEOUT_MS / 60_000)}m${DRY_RUN ? ", DRY_RUN" : ""}${STATE_RESOLVED.sandboxed ? `, state=${STATE_DIR}` : ""})`)
  await ensureBoardColumns(projects)
  // громкое замечание резолюции state: авто-изоляция ветки или неудавшийся
  // git-детект — в лог и stderr (journald), чтобы не было тихих сюрпризов
  emitNotice(STATE_RESOLVED.notice, log)
  const state = loadState()
  log(`восстановлено задач: ${Object.keys(state.tasks).length}`)
  // старт-свип окружений: сироты без записи в state и старый attic — только
  // прод (песочница/DRY_RUN не трогают реальный ENV_ROOT)
  if (!DRY_RUN && !STATE_RESOLVED.sandboxed) {
    const sweep = sweepEnvs(envDeps(), {
      root: ENV_ROOT,
      // имена каталогов прошли sanitizeRowId (envPaths) — сравниваем в том же
      // нормализованном виде (изменится формат id — живой env не снесётся)
      activeRowIds: new Set(Object.keys(state.tasks).map(sanitizeRowId)),
      nowMs: Date.now(),
      maxAgeMs: ENV_MAX_AGE_MS,
    })
    log(`старт-свип env (${ENV_ROOT}): снесено сирот=${sweep.sweptEnvs.length}, attic=${sweep.sweptAttic.length}`)
    // GC легаси-корней ворктри: старт-проход по всем проектам
    for (const [name, cfg] of Object.entries(projects)) {
      try {
        await gcLegacyRoots(name, cfg, state)
      } catch (e) {
        log(`gc[${name}]: ${e}`)
      }
    }
  }
  while (true) {
    for (const [name, cfg] of Object.entries(projects)) {
      if (drain.isStopping()) break
      try {
        await tickProject(state, name, cfg)
      } catch (e) {
        log(`tick[${name}]: ${e}`)
      }
    }
    tickCount++
    if (tickCount % 10 === 0) rotateLog()
    // GC легаси-ворктри: раз в GC_EVERY_TICKS тиков (≈час при poll 180s)
    if (!DRY_RUN && !STATE_RESOLVED.sandboxed && tickCount % GC_EVERY_TICKS === 0) {
      for (const [name, cfg] of Object.entries(projects)) {
        if (drain.isStopping()) break
        try {
          await gcLegacyRoots(name, cfg, state)
        } catch (e) {
          log(`gc[${name}]: ${e}`)
        }
      }
    }
    if (ONCE) {
      log("ONCE-режим: тик выполнен — выходим")
      process.exit(0)
    }
    if (drain.isStopping()) {
      log("стоп-сигнал: проход доехал — выходим")
      process.exit(0)
    }
    await drain.sleep(POLL_SEC * 1000)
  }
}

if (import.meta.main) {
  void main()
}
