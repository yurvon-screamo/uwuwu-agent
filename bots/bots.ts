// bots.ts — супервизор раннера uwuwu-bots: бот описывается доком реестра
// (папка «Боты» проекта uwuwu), раннер сам его запускает и держит — механизм
// в стиле крона, без кода на нового бота (постановка 20261112152504).
//
// Каждый бот — отдельный ребёнок (bots/bot-child.ts) на обкатанном ядре
// lib/hub.ts: изоляция сбоев, гашение одного не роняет остальных, вечные
// сессии живут в opencode.db переживая рестарты. Живость ребёнка — его
// per-бот lock (pid из lib/single-instance): усыновление сирот после
// рестарта супервизора бесплатно.
//
// Тик: скан реестра (MCP organize-ноды + kernel exportMdContent) → диф
// (bots-lib.decideActions) → spawn/restart/stop/def-update. Отказ скана —
// заморозка состава (детей не поднимать и не гасить по stale-данным), диф
// продолжается при ожила ядра.
//
// Запуск: systemd --user (uwuwu-bots.service, EnvironmentFile= — носитель
// токена перечитывается рестартом юнита, координация 20261112195849).

import { mkdirSync, appendFileSync, readFileSync, renameSync, writeFileSync } from "node:fs"
import { homedir, hostname } from "node:os"
import { dirname, join } from "node:path"
import { mcpConfigured, tool as mcpTool } from "../lib/platform"
import { checkNodePinning, failPinnedStart } from "../lib/node-pinning"
import { acquireLock, makeDrain } from "../lib/single-instance"
import {
  decideActions, defTargetDirectory, hashDjb2, liveSessionDirectory, mergeCache, parseBotDoc, parseModel,
  type BotObservation, type BotSpec, type RegistryCacheEntry,
} from "./bots-lib"

type PlatformTool = <T = Record<string, unknown>>(name: string, args: Record<string, unknown>) => Promise<T>

const HERE = import.meta.dir

const RUN_ROOT = process.env.UWUWU_BOTS_HOME ?? homedir()
const STATE_DIR = join(RUN_ROOT, ".local/state/uwuwu-bots")
const SUPER_STATE = join(STATE_DIR, "supervisor.json")
/** Корень per-бот каталогов (hub.json/agent.md/bot-meta.json ребёнка). */
const BOTS_DIR = join(RUN_ROOT, ".local/share/uwuwu-bots")
const LOG_PATH = join(RUN_ROOT, ".local/share/opencode/log/uwuwu-bots.log")
/** Регистрация фонового opencode-сервиса (паритет lib/hub.ts). */
const SERVICE_REG = join(homedir(), ".local/state/opencode/service.json")

const TICK_SEC = Math.max(5, Number(process.env.UWUWU_BOTS_TICK_SEC ?? 60) || 60)

const log = (m: string): void => {
  try {
    mkdirSync(dirname(LOG_PATH), { recursive: true })
    appendFileSync(LOG_PATH, `${new Date().toISOString()} ${m}\n`)
  } catch {}
}

// ——— opencode REST (service.json; паритет lib/hub restCall — нужен
//  супервизору только для seed-контракта: directory живой сессии) ———

async function ocRest(path: string): Promise<unknown> {
  const reg = JSON.parse(readFileSync(SERVICE_REG, "utf-8")) as { url?: string; password?: string }
  const base = (reg.url ?? "").replace("0.0.0.0", "127.0.0.1")
  const auth = `Basic ${Buffer.from(`opencode:${reg.password ?? ""}`).toString("base64")}`
  const res = await fetch(`${base}/api${path}`, {
    headers: { authorization: auth, "content-type": "application/json" },
    signal: AbortSignal.timeout(15_000),
  })
  const text = await res.text()
  if (!res.ok) throw Object.assign(new Error(`HTTP ${res.status}: ${text.slice(0, 120)}`), { status: res.status })
  if (text.trim() === "") return {}
  return (JSON.parse(text) as { data?: unknown }).data ?? JSON.parse(text)
}

// ——— kernel export (env-токен; паритет cli export_md_by_id) ———

async function kernelExportMd(docId: string): Promise<string> {
  const token = process.env.UWUWU_SIYUAN_TOKEN ?? ""
  const url = process.env.UWUWU_SIYUAN_URL ?? "http://127.0.0.1:6806"
  const res = await fetch(`${url}/api/export/exportMdContent`, {
    method: "POST",
    headers: { authorization: `Token ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ id: docId }),
    signal: AbortSignal.timeout(15_000),
  })
  const j = await res.json() as { code?: number; msg?: string; data?: { content?: string } }
  if (j.code !== 0) throw new Error(`kernel export ${docId}: ${j.msg ?? `HTTP ${res.status}`}`)
  return j.data?.content ?? ""
}

// ——— скан реестра ———

interface OrganizeNode {
  id?: string
  type?: string
  data?: string
  parentId?: string
}

/** Контейнер реестра: док «Боты» — ребёнок папки проекта uwuwu (доки-контейнеры
 *  SiYuan, паритет триажа с досками «Задачи …»; organize-папкой не является).
 *  Один list_organize_nodes на тик — ноды подаются снаружи. */
async function botsContainerId(tool: PlatformTool, nodes: OrganizeNode[]): Promise<string | null> {
  const projects = nodes.find((n) => n.type === "folder" && n.data === "Projects" && n.parentId === null)
  if (projects === undefined) return null
  const uwuwu = nodes.find((n) => n.type === "folder" && n.data === "uwuwu" && n.parentId === projects.id)
  if (uwuwu === undefined) return null
  const candidates = nodes.filter((n) => n.type === "doc" && n.parentId === uwuwu.id && typeof n.id === "string")
  for (const candidate of candidates) {
    const info = await tool<{ title?: string }>("get_doc", { docId: candidate.id as string })
    if (info.title === "Боты") return candidate.id as string
  }
  return null
}

const normalizeAgentName = (slug: string): string => {
  const base = slug.toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "")
  return base !== "" ? base : `bot-${hashDjb2(slug)}`
}

/** Результат скана: наблюдения по докам + признаки недостоверности скана
 *  (контракт заморозки — см. tick). */
interface ScanResult {
  observations: Map<string, BotObservation>
  /** контейнер «Боты» не найден при живом ядре: переименование/перенос
   *  владельцем или лаг организ-индекса — НЕ доказательство сноса ботов */
  containerMissing: boolean
  /** docId, чтение/парсинг которых упало (док из кэша → слаг замораживается) */
  failedDocIds: Set<string>
}

async function scanRegistry(tool: PlatformTool): Promise<ScanResult> {
  const r = await tool<{ nodes?: OrganizeNode[] }>("list_organize_nodes", {})
  const nodes = r.nodes ?? []
  const containerId = await botsContainerId(tool, nodes)
  if (containerId === null) return { observations: new Map(), containerMissing: true, failedDocIds: new Set() }
  const docs = nodes.filter((n) => n.type === "doc" && n.parentId === containerId && typeof n.id === "string")
  const observations = new Map<string, BotObservation>()
  const failedDocIds = new Set<string>()
  for (const doc of docs) {
    try {
      const info = await tool<{ title?: string; updatedAt?: string }>("get_doc", { docId: doc.id })
      const md = await kernelExportMd(doc.id as string)
      const parsed = parseBotDoc(doc.id as string, md)
      const model = parsed.model !== undefined ? parseModel(parsed.model) : undefined
      if (model === undefined || model === null) throw new Error(`bot.model пуст или не распарсился (provider/id[#variant]): «${parsed.model ?? ""}»`)
      const spec: BotSpec = {
        slug: parsed.slug,
        docId: doc.id as string,
        token: parsed.token,
        owner: parsed.owner,
        model,
        enabled: parsed.enabled,
        def: parsed.def,
        ...(parsed.workspace !== undefined && parsed.workspace !== "" ? { workspace: parsed.workspace } : {}),
      }
      // коллизия слагов (два дока «Бот: x») — громко: слаг — ключ жизненного цикла
      if (observations.has(spec.slug)) throw new Error(`дубликат слага «${spec.slug}» в реестре`)
      observations.set(spec.slug, { spec, updatedAt: info.updatedAt ?? "" })
    } catch (e) {
      log(`реестр: док ${doc.id}: ${e} — скип`)
      failedDocIds.add(doc.id as string)
    }
  }
  return { observations, containerMissing: false, failedDocIds }
}

// ——— per-бот каталог: материал ребёнка ———

const botDirOf = (slug: string): string => join(BOTS_DIR, slug)

/** Атомарная запись; mode — для секретов (hub.json: 0600 — токен не читаем
 *  others на multi-user хосте). */
function writeAtomic(path: string, content: string, mode?: number): void {
  mkdirSync(dirname(path), { recursive: true })
  const tmp = `${path}.tmp`
  writeFileSync(tmp, content, mode !== undefined ? { mode } : undefined)
  renameSync(tmp, path)
}

interface ChildMeta {
  slug: string
  agentName: string
  sessionTitle: string
  workspace?: string
}

function materializeBotDir(obs: BotObservation): string {
  const dir = botDirOf(obs.spec.slug)
  writeAtomic(join(dir, "hub.json"), JSON.stringify({
    token: obs.spec.token,
    owner: obs.spec.owner,
    model: obs.spec.model,
  }, null, 2), 0o600)
  writeAtomic(join(dir, "agent.md"), obs.spec.def)
  const meta: ChildMeta = {
    slug: obs.spec.slug,
    agentName: normalizeAgentName(obs.spec.slug),
    sessionTitle: `Бот: ${obs.spec.slug}`,
    ...(obs.spec.workspace !== undefined ? { workspace: obs.spec.workspace } : {}),
  }
  writeAtomic(join(dir, "bot-meta.json"), JSON.stringify(meta, null, 2))
  return dir
}

/** Синх дефа в directory живой сессии: деф резолвится сервером из location
 *  сессии — при переехавшей вечной сессии материализуем agent.md и там
 *  (расхождение с workspace спеки — warning). Вызывается на spawn/restart
 *  И на def-update — иначе горячая правка молча перестаёт доезжать
 *  seeded-сессии (ревью Common-2). null — сессии нет. */
async function syncDefIntoSessionDirectory(obs: BotObservation): Promise<string | null> {
  const statePath = join(RUN_ROOT, ".local/state", `uwuwu-bots-${obs.spec.slug}`, "state.json")
  let session: string | undefined
  try {
    const st = JSON.parse(readFileSync(statePath, "utf-8")) as { session?: string }
    session = typeof st.session === "string" && st.session.startsWith("ses_") ? st.session : undefined
  } catch { return null }
  if (session === undefined) return null
  const dir = liveSessionDirectory(await ocRest(`/session/${session}`).catch(() => null))
  if (dir === null) return null
  const { directory, warning } = defTargetDirectory(obs.spec.workspace, dir)
  if (warning !== undefined) log(`бот ${obs.spec.slug}: ${warning}`)
  if (directory === "") return null
  const dst = join(directory, ".opencode/agents", `${normalizeAgentName(obs.spec.slug)}.md`)
  writeAtomic(dst, obs.spec.def)
  log(`бот ${obs.spec.slug}: деф сессии материализован в ${dst}`)
  return directory
}

// ——— живость и управление детьми ———

function childPid(slug: string): number | null {
  try {
    const pid = Number(readFileSync(join(RUN_ROOT, ".local/state", `uwuwu-bots-${slug}`, "lock/pid"), "utf-8").trim())
    if (!Number.isInteger(pid) || pid <= 0) return null
    process.kill(pid, 0)
    return pid
  } catch {
    return null
  }
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms))

async function stopChild(slug: string, pid: number): Promise<void> {
  try { process.kill(pid, "SIGTERM") } catch { return }
  for (let waited = 0; waited < 30_000; waited += 500) {
    try { process.kill(pid, 0); await sleep(500) } catch {
      log(`бот ${slug}: ребёнок ${pid} погашен`)
      return
    }
  }
  try { process.kill(pid, "SIGKILL") } catch {}
  log(`бот ${slug}: ребёнок ${pid} не дошёл за 30с — SIGKILL`)
}

function spawnChild(slug: string): void {
  const proc = Bun.spawn(["bun", join(HERE, "bot-child.ts"), slug], {
    cwd: HERE,
    stdout: "inherit",
    stderr: "inherit",
  })
  log(`бот ${slug}: ребёнок спавнен (pid ${proc.pid})`)
}

// ——— state супервизора (кэш реестра) ———

interface SupervisorState {
  cache: Record<string, RegistryCacheEntry>
}
let state: SupervisorState = { cache: {} }
try { state = { cache: (JSON.parse(readFileSync(SUPER_STATE, "utf-8")) as SupervisorState).cache ?? {} } } catch {}
const save = (): void => {
  writeAtomic(SUPER_STATE, JSON.stringify(state, null, 1))
}

// ——— исполнение действий тика ———

async function spawnBot(obs: BotObservation): Promise<void> {
  materializeBotDir(obs)
  await syncDefIntoSessionDirectory(obs)
  spawnChild(obs.spec.slug)
}

async function restartBot(obs: BotObservation, reason: string): Promise<void> {
  log(`бот ${obs.spec.slug}: ${reason} — рестарт ребёнка`)
  const pid = childPid(obs.spec.slug)
  if (pid !== null) await stopChild(obs.spec.slug, pid)
  materializeBotDir(obs)
  await syncDefIntoSessionDirectory(obs)
  spawnChild(obs.spec.slug)
}

async function stopBot(action: { slug: string }): Promise<void> {
  log(`бот ${action.slug}: док снесён или enabled: off — гашу ребёнка (вечная сессия в opencode.db остаётся)`)
  const pid = childPid(action.slug)
  if (pid !== null) await stopChild(action.slug, pid)
}

async function updateDef(obs: BotObservation): Promise<void> {
  writeAtomic(join(botDirOf(obs.spec.slug), "agent.md"), obs.spec.def)
  // seeded-сессия резолвит деф из своего directory (≠ workspace): без этого
  // синха горячая правка молча перестаёт доезжать (ревью Common-2)
  await syncDefIntoSessionDirectory(obs)
  log(`бот ${obs.spec.slug}: методичка обновлена — ребёнок подхватит на следующем ходе`)
}

async function executeActions(actions: ReturnType<typeof decideActions>, scanned: Map<string, BotObservation>): Promise<void> {
  for (const action of actions) {
    // stop не требует наблюдения (снос дока / enabled: off по кэшу+lock);
    // остальные actions порождаются только из scanned — obs обязана быть
    if (action.kind === "stop") {
      await stopBot(action)
      continue
    }
    const obs = scanned.get(action.slug)
    if (obs === undefined) continue
    if (action.kind === "spawn") await spawnBot(obs)
    else if (action.kind === "restart") await restartBot(obs, action.reason)
    else await updateDef(obs)
  }
}

// ——— тик и старт ———

const drain = makeDrain()

async function tick(tool: PlatformTool): Promise<void> {
  let scan: ScanResult
  try {
    scan = await scanRegistry(tool)
  } catch (e) {
    // заморозка состава: ядро лежит — детей не поднимать и не гасить по
    // stale-данным; диф продолжится при ожила ядра
    log(`тик: скан реестра не удался (${e}) — заморозка состава до следующего тика`)
    return
  }
  const cacheSlugs = Object.keys(state.cache)
  // живость по всем известным слагам (кэш + скан), не только успешного скана:
  // иначе живой ребёнок снесённого дока невидим дифу и не гасится (ревью High)
  const liveKnown = new Set([...cacheSlugs, ...scan.observations.keys()].filter((s) => childPid(s) !== null))
  // контракт заморозки (ревью Common-1): недостоверный скан при живом ядре —
  // НЕ доказательство «док снесён»; заморозить затронутых, не гасить живых
  const frozen = new Set<string>()
  if (scan.containerMissing) {
    if (cacheSlugs.length > 0 || liveKnown.size > 0) {
      log(`тик: контейнер «Боты» не найден при живом ядре и известных ботах (${cacheSlugs.join(", ") || "—"}) — заморозка состава (лаг индекса/перенос владельцем?)`)
      for (const slug of new Set([...cacheSlugs, ...liveKnown])) frozen.add(slug)
    }
  } else {
    for (const docId of scan.failedDocIds) {
      const slug = cacheSlugs.find((s) => state.cache[s].docId === docId)
      if (slug !== undefined) {
        frozen.add(slug)
        log(`тик: бот ${slug}: док ${docId} не читается — заморозка на последнем известном составе`)
      }
    }
  }
  const actions = decideActions(state.cache, [...scan.observations.values()], liveKnown, frozen)
  await executeActions(actions, scan.observations)
  // кэш «последнего известного состава» — мерж, не замещение: записи
  // замороженных/недочитанных слагов переживают недостоверный тик (ревью Common-2)
  state = { cache: mergeCache(state.cache, [...scan.observations.values()], frozen) }
  save()
}

const main = async (): Promise<void> => {
  if (!mcpConfigured()) {
    console.error("uwuwu-bots: нет UWUWU_SIYUAN_* — скан реестра невозможен, раннер не стартует")
    process.exit(1)
  }
  if (!acquireLock({ stateDir: STATE_DIR, onStop: () => drain.stop() })) {
    console.error("другой живой инстанс uwuwu-bots держит lock — выходим")
    process.exit(1)
  }
  mkdirSync(dirname(LOG_PATH), { recursive: true })
  const pinning = await checkNodePinning(mcpTool, {
    singletonDaemon: "bots",
    envNodeName: process.env.UWUWU_NODE_NAME,
    hostname: hostname(),
  })
  failPinnedStart(pinning)
  log(`uwuwu-bots: старт (нода ${pinning.nodeName}, тик ${TICK_SEC}s, per-бот каталоги ${BOTS_DIR})`)

  while (true) {
    try {
      if (drain.isStopping()) break
      await tick(mcpTool)
    } catch (e) {
      log(`тик: ${e}`)
    }
    await drain.sleep(TICK_SEC * 1000)
  }
  log("uwuwu-bots: останов")
}

void main()
