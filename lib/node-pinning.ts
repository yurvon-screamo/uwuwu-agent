// node-pinning.ts — пиннинг демонов за нодами контура (ADR-076): док
// «Node-pinning» в проекте uwuwu — единственный источник истины топологии.
// Демон при старте сверяет свой скоуп с докой: пересечение или незапиненный
// конфиг в проде — громкий отказ старта. Песочница смоук-тиков (ADR-059)
// unknown-проекты пропускает — их в доке нет и не должно быть.
export const PINNING_DOC_TITLE = "Node-pinning"
export const PINNING_PROJECT = "uwuwu"

export interface PinningDoc {
  /** нода → пиненые за ней проекты фабрик */
  factories: Record<string, string[]>
  /** singleton-демон (один инстанс на контур) → нода */
  singles: Record<string, string>
  /** нод-локальные демоны (легитимны на любой ноде) → ноды; справочно,
   *  валидаторами не читается */
  local: Record<string, string[]>
}

/** Форма tool lib/platform (локальное объявление — паттерн cron-lib). */
export type PinningTool = <T = Record<string, unknown>>(name: string, args: Record<string, unknown>) => Promise<T>

const SECTION_FACTORIES = "фабрики"
const SECTION_SINGLES = "одиночные демоны"
const SECTION_LOCAL_PREFIX = "нод-локальные"

const LIST_LINE = /^- ([^:]+): (.+)$/

/** Норм-форма ключа (нода/демон/проект): trim + lowercase + срез format-
 *  символов — док правится владельцем в UI, регистр и ZWSP-артефакты ядра
 *  (инвентарь round-trip, класс \p{Cf} — паритет normalizeBodyMd) не контракт. */
function normalizeKey(raw: string): string {
  return raw.replace(/\p{Cf}/gu, "").trim().toLowerCase()
}

function parseListValues(raw: string): string[] {
  return raw.split(",").map(normalizeKey).filter((v) => v !== "")
}

/** Парс markdown пиннинг-дока (export_doc_markdown). Строки вне известных
 *  `##`-секций игнорируются (обёртка экспорта, проза владельца); неизвестная
 *  `##`-секция, дубликат ключа или singles с несколькими нодами — throw
 *  (формат — контракт, молчаливый пропуск прячет опечатку владельца). */
export function parsePinningDoc(md: string): PinningDoc {
  const doc: PinningDoc = { factories: {}, singles: {}, local: {} }
  let section: string | null = null
  for (const line of md.split("\n")) {
    const heading = line.match(/^## (.+)$/)
    if (heading !== null) {
      section = resolveSection(heading[1])
      continue
    }
    if (section === null) continue
    const item = line.match(LIST_LINE)
    if (item === null) continue
    const key = normalizeKey(item[1])
    if (key === "") throw new Error(`пиннинг-док: пустой ключ в строке «${line.trim()}»`)
    appendListItem(doc, section, key, item[2])
  }
  if (Object.keys(doc.factories).length === 0) {
    throw new Error(`пиннинг-док: секция «## Фабрики» не найдена или пуста`)
  }
  return doc
}

/** Заголовок `##` → канон секции; неизвестный — throw. */
function resolveSection(rawTitle: string): string {
  const title = normalizeKey(rawTitle)
  if (title === SECTION_FACTORIES) return SECTION_FACTORIES
  if (title === SECTION_SINGLES) return SECTION_SINGLES
  if (title.startsWith(SECTION_LOCAL_PREFIX)) return SECTION_LOCAL_PREFIX
  throw new Error(`пиннинг-док: неизвестная секция «${rawTitle.trim()}» — формат: Фабрики, Одиночные демоны, Нод-локальные`)
}

/** Строка списка `- ключ: значения` → запись секции (дубликаты и битые
 *  значения — throw). */
function appendListItem(doc: PinningDoc, section: string, key: string, rawValues: string): void {
  if (section === SECTION_SINGLES) {
    const nodes = parseListValues(rawValues)
    if (nodes.length !== 1) throw new Error(`пиннинг-док: «${key}» — одна строка = одна нода, получено ${nodes.length}`)
    if (key in doc.singles) throw new Error(`пиннинг-док: дубликат «${key}» в «${SECTION_SINGLES}»`)
    doc.singles[key] = nodes[0]
    return
  }
  const values = parseListValues(rawValues)
  if (values.length === 0) throw new Error(`пиннинг-док: «${key}» — пустой список значений`)
  const target = section === SECTION_FACTORIES ? doc.factories : doc.local
  if (key in target) throw new Error(`пиннинг-док: дубликат «${key}» в «${section}»`)
  target[key] = values
}

/** Нарушения фабрики: перепин (проект конфига пинен за другую ноду) — всегда;
 *  unknown-проект (нигде не пинен) — только в проде (песочница смоук-тиков
 *  гоняет тестовые доски, в доке их нет и не должно быть, ADR-059). */
export function validateFactory(pinning: PinningDoc, nodeName: string, configProjects: readonly string[], sandboxed: boolean): string[] {
  const node = normalizeKey(nodeName)
  const violations: string[] = []
  for (const project of configProjects) {
    const owner = Object.entries(pinning.factories).find(([, projects]) => normalizeList(projects).includes(normalizeKey(project)))
    if (owner === undefined) {
      if (!sandboxed) violations.push(`проект «${project}» не запинен в доке «Node-pinning» — прод-фабрика с незапиненным проектом не стартует (правь док до старта)`)
      continue
    }
    if (owner[0] !== node) violations.push(`проект «${project}» пинен за ноду «${owner[0]}», не за «${node}» — пересечение скоупов фабрик`)
  }
  return violations
}

function normalizeList(projects: readonly string[]): string[] {
  return projects.map(normalizeKey)
}

/** Нарушения singleton-демона: строка «демон: нода» обязательна — демона без
 *  пина не бывает (у singleton нет конфига скоупа, нечему расходиться: строка
 *  либо есть и сверяется, либо её нет и это отказ). */
export function validateSingleton(pinning: PinningDoc, daemon: string, nodeName: string): string[] {
  const node = normalizeKey(nodeName)
  const pinned = pinning.singles[normalizeKey(daemon)]
  if (pinned === undefined) return [`демон «${daemon}» не запинен в доке «Node-pinning» (секция «Одиночные демоны») — демон без пина не стартует`]
  if (pinned !== node) return [`демон «${daemon}» пинен за ноду «${pinned}», не за «${node}» — второй экземпляр на контуре недопустим`]
  return []
}

/** Имя ноды: env-оверрайд или hostname (lowercase). Пустой env — не ошибка:
 *  hostname самодостаточен, env нужен только когда hostname ≠ имени в доке. */
export function nodeNameFromEnv(envValue: string | undefined, hostname: string): string {
  const fromEnv = envValue?.trim()
  return fromEnv !== undefined && fromEnv !== "" ? normalizeKey(fromEnv) : hostname.trim().toLowerCase()
}

export interface PinningCheck {
  nodeName: string
  violations: string[]
  /** unknown-проекты, пропущенные песочницей (для лога демона) */
  sandboxSkipped: string[]
}

/** Старт-сверка демона: резолв дока + валидация скоупа. Ровно одно из
 *  полей скоупа — другая комбинация = ошибка программирования вызова
 *  (тихий no-op неразличим снаружи и противоположен fail-loud духу
 *  фичи). Вызывающий демон сам решает судьбу (прод-паттерн:
 *  failPinnedStart при непустых violations). */
export async function checkNodePinning(tool: PinningTool, opts: {
  singletonDaemon?: string
  factoryProjects?: readonly string[]
  /** только фабрике: песочница смоук-тиков пропускает unknown-проекты (ADR-059) */
  sandboxed?: boolean
  envNodeName?: string
  hostname: string
}): Promise<PinningCheck> {
  const scopes = [opts.singletonDaemon !== undefined, opts.factoryProjects !== undefined].filter(Boolean).length
  if (scopes !== 1) {
    throw new Error(`checkNodePinning: ровно одно из singletonDaemon/factoryProjects обязательно, получено ${scopes} — вызов молча ничего бы не проверил`)
  }
  const pinning = await resolvePinningDoc(tool)
  const nodeName = nodeNameFromEnv(opts.envNodeName, opts.hostname)
  let violations: string[] = []
  let sandboxSkipped: string[] = []
  if (opts.singletonDaemon !== undefined) {
    violations = validateSingleton(pinning, opts.singletonDaemon, nodeName)
  } else {
    const sandboxed = opts.sandboxed === true
    violations = validateFactory(pinning, nodeName, opts.factoryProjects as readonly string[], sandboxed)
    sandboxSkipped = sandboxed
      ? (opts.factoryProjects as readonly string[]).filter((p) => !normalizeList(pinnedFactoryProjects(pinning)).includes(normalizeKey(p)))
      : []
  }
  return { nodeName, violations, sandboxSkipped }
}

/** Прод-паттерн демонов: непустые violations — отказ старта (stderr) и
 *  exit 1; пустые — no-op (демон продолжает свои строки подтверждения). */
export function failPinnedStart(check: PinningCheck, print: (line: string) => void = console.error): void {
  if (check.violations.length === 0) return
  print(`пиннинг: отказ старта (нода ${check.nodeName}):\n- ${check.violations.join("\n- ")}`)
  process.exit(1)
}

function pinnedFactoryProjects(pinning: PinningDoc): string[] {
  return Object.values(pinning.factories).flat()
}

/** Дискавери дока по организ-дереву: папка проекта uwuwu (паттерн
 *  findTaskBoards фабрики), среди её детей — док с точным заголовком
 *  «Node-pinning»; контент — export_doc_markdown (паттерн resolveBody
 *  крона). Scope папкой — тёзка вне проекта uwuwu не источник истины.
 *  Throw вместо null: немой пропуск = демон без пиннинга. */
async function resolvePinningDoc(tool: PinningTool): Promise<PinningDoc> {
  const r = await tool<{ nodes?: { id?: string; type?: string; data?: string; parentId?: string }[] }>("list_organize_nodes", {})
  const nodes = r.nodes ?? []
  const folder = nodes.find((n) => n.type === "folder" && normalizeKey(n.data ?? "") === PINNING_PROJECT)
  if (folder === undefined || typeof folder.id !== "string") {
    throw new Error(`пиннинг-док не найден: нет папки проекта «${PINNING_PROJECT}» в организ-дереве`)
  }
  const docIds = nodes
    .filter((n) => n.type === "doc" && n.parentId === folder.id && typeof n.data === "string")
    .map((n) => n.data as string)
  const titles = await Promise.all(docIds.map(async (docId) => {
    const d = await tool<{ title?: string }>("get_doc", { docId })
    return { docId, title: d.title ?? "" }
  }))
  const exact = titles.filter((t) => t.title === PINNING_DOC_TITLE)
  if (exact.length === 0) throw new Error(`пиннинг-док не найден: в папке «${PINNING_PROJECT}» нет дока «${PINNING_DOC_TITLE}» (uwuwu-cli read "${PINNING_PROJECT}/${PINNING_DOC_TITLE}")`)
  if (exact.length > 1) throw new Error(`пиннинг-док неоднозначен: ${exact.length} доков с заголовком «${PINNING_DOC_TITLE}» в «${PINNING_PROJECT}» — снеси дубли`)
  const exported = await tool<{ exists?: boolean; markdown?: string }>("export_doc_markdown", { docId: exact[0].docId })
  if (exported.exists !== true) throw new Error(`пиннинг-док «${PINNING_DOC_TITLE}» не экспортируется (exists:false)`)
  return parsePinningDoc(exported.markdown ?? "")
}
