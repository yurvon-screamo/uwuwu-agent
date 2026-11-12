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
  decideActions, defTargetDirectory, liveSessionDirectory, parseBotDoc, parseModel, runtimeHashOf,
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
 *  null — контейнера ещё нет (реестр пуст). */
async function botsContainerId(tool: PlatformTool): Promise<string | null> {
  const r = await tool<{ nodes?: OrganizeNode[] }>("list_organize_nodes", {})
  const nodes = r.nodes ?? []
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
  if (base !== "") return base
  let h = 5381
  for (let i = 0; i < slug.length; i++) h = ((h << 5) + h + slug.charCodeAt(i)) | 0
  return `bot-${(h >>> 0).toString(36)}`
}

async function scanRegistry(tool: PlatformTool): Promise<Map<string, BotObservation>> {
  const containerId = await botsContainerId(tool)
  if (containerId === null) return new Map()
  const r = await tool<{ nodes?: OrganizeNode[] }>("list_organize_nodes", {})
  const docs = (r.nodes ?? []).filter((n) => n.type === "doc" && n.parentId === containerId && typeof n.id === "string")
  const out = new Map<string, BotObservation>()
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
      if (out.has(spec.slug)) throw new Error(`дубликат слага «${spec.slug}» в реестре`)
      out.set(spec.slug, { spec, updatedAt: info.updatedAt ?? "" })
    } catch (e) {
      log(`реестр: док ${doc.id}: ${e} — скип`)
    }
  }
  return out
}

// ——— per-бот каталог: материал ребёнка ———

const botDirOf = (slug: string): string => join(BOTS_DIR, slug)

function writeAtomic(path: string, content: string): void {
  mkdirSync(dirname(path), { recursive: true })
  const tmp = `${path}.tmp`
  writeFileSync(tmp, content)
  renameSync(tmp, path)
}

interface ChildMeta {
  slug: string
  agentName: string
  sessionTitle: string
  workspace?: string
  model?: { providerID: string; id: string; variant?: string }
}

function materializeBotDir(obs: BotObservation): string {
  const dir = botDirOf(obs.spec.slug)
  writeAtomic(join(dir, "hub.json"), JSON.stringify({
    token: obs.spec.token,
    owner: obs.spec.owner,
    ...(obs.spec.model !== undefined ? { model: obs.spec.model } : {}),
  }, null, 2))
  writeAtomic(join(dir, "agent.md"), obs.spec.def)
  const meta: ChildMeta = {
    slug: obs.spec.slug,
    agentName: normalizeAgentName(obs.spec.slug),
    sessionTitle: `Бот: ${obs.spec.slug}`,
    ...(obs.spec.workspace !== undefined ? { workspace: obs.spec.workspace } : {}),
    ...(obs.spec.model !== undefined ? { model: obs.spec.model } : {}),
  }
  writeAtomic(join(dir, "bot-meta.json"), JSON.stringify(meta, null, 2))
  return dir
}

/** Seed-контракт: деф резолвится сервером из directory сессии — при
 *  переехавшей вечной сессии материализуем agent.md в её directory (расхождение
 *  с workspace спеки — warning). Возвращает directory, куда ребёнок положит
 *  деф сам (syncAgentDef — workspace), или null, если сессии нет. */
async function seedDefIntoSessionDirectory(obs: BotObservation): Promise<string | null> {
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
  log(`бот ${obs.spec.slug}: деф seeded-сессии материализован в ${dst}`)
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

async function executeActions(actions: ReturnType<typeof decideActions>, scanned: Map<string, BotObservation>): Promise<void> {
  for (const action of actions) {
    const obs = scanned.get(action.slug)
    if (action.kind !== "stop" && obs === undefined) continue
    const pid = childPid(action.slug)
    if (action.kind === "spawn") {
      if (obs === undefined) continue
      materializeBotDir(obs)
      await seedDefIntoSessionDirectory(obs)
      spawnChild(action.slug)
    } else if (action.kind === "restart") {
      log(`бот ${action.slug}: ${action.reason} — рестарт ребёнка`)
      if (pid !== null) await stopChild(action.slug, pid)
      if (obs !== undefined) materializeBotDir(obs)
      if (obs !== undefined) await seedDefIntoSessionDirectory(obs)
      spawnChild(action.slug)
    } else if (action.kind === "stop") {
      log(`бот ${action.slug}: док снесён или enabled: off — гашу ребёнка (вечная сессия в opencode.db остаётся)`)
      if (pid !== null) await stopChild(action.slug, pid)
    } else if (action.kind === "def-update") {
      if (obs !== undefined) {
        const dir = botDirOf(action.slug)
        writeAtomic(join(dir, "agent.md"), obs.spec.def)
        log(`бот ${action.slug}: методичка обновлена в per-бот каталоге — ребёнок подхватит на следующем ходе`)
      }
    }
  }
}

// ——— тик и старт ———

const drain = makeDrain()

async function tick(tool: PlatformTool): Promise<void> {
  let scanned: Map<string, BotObservation>
  try {
    scanned = await scanRegistry(tool)
  } catch (e) {
    // заморозка состава: ядро лежит — детей не поднимать и не гасить по
    // stale-данным; диф продолжится при ожила ядра
    log(`тик: скан реестра не удался (${e}) — заморозка состава до следующего тика`)
    return
  }
  const live = new Set<string>()
  for (const slug of scanned.keys()) {
    if (childPid(slug) !== null) live.add(slug)
  }
  const actions = decideActions(state.cache, [...scanned.values()], live)
  await executeActions(actions, scanned)
  const nextCache: Record<string, RegistryCacheEntry> = {}
  for (const [slug, obs] of scanned) {
    nextCache[slug] = {
      docId: obs.spec.docId,
      updatedAt: obs.updatedAt,
      runtimeHash: runtimeHashOf(obs.spec.token, obs.spec.owner, obs.spec.model, obs.spec.workspace, obs.spec.enabled),
    }
  }
  const before = JSON.stringify(state.cache)
  const after = JSON.stringify(nextCache)
  if (before !== after) {
    state = { cache: nextCache }
    save()
  }
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
