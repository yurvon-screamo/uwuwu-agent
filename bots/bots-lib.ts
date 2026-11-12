// bots-lib.ts — чистые функции реестра ботов uwuwu-bots: парсинг док-спеки
// бота (frontmatter + методичка), диф жизненного цикла (что делать с
// ребёнком на тике), выбор directory для материализации дефа (seed-контракт
// переехавшей вечной сессии). Тесты — bots-lib.test.ts, без сети.

import type { HubModel } from "../lib/hub"

/** Док-спека бота из реестра (папка «Боты» проекта uwuwu). */
export interface BotSpec {
  /** слаг бота = тайтл дока после префикса «Бот: » (ключ жизненного цикла) */
  slug: string
  /** docId дока реестра (диф по updatedAt, заморозка кэша) */
  docId: string
  token: string
  /** username владельца ТГ (classifyUpdate ядра — private-only) */
  owner: string
  /** модель сессии — обязательна (явная спека без скрытых дефолтов) */
  model: HubModel
  /** каталог сессий и дефа; undefined — изоляция раннера per-бот */
  workspace?: string
  /** false = бот погашен (ребёнка гасим, вечная сессия в opencode.db живёт) */
  enabled: boolean
  /** методичка агента — тело дока (материализуется в деф per-бот каталога) */
  def: string
}

export interface BotObservation {
  spec: BotSpec
  /** updatedAt дока реестра (ядро, yyyyMMddHHmmss) — вход дифа методички */
  updatedAt: string
}

// ——— парсинг док-спеки ———

const SPEC_PREFIX = "Бот: "
/** Секция спеки в теле док-спеки: yaml-фенс с блоком `bot:` (frontmatter
 *  SiYuan не хранит — вложенные ключи ядра выкидывает, живая проба 13.11). */
const SPEC_HEADING = "## Спека"

export interface ParsedBotDoc {
  slug: string
  token: string
  owner: string
  model?: string
  workspace?: string
  enabled: boolean
  def: string
}

/** Строка value без кавычек и trim (владельцу разрешены кавычки — отрезаем). */
const unquote = (raw: string): string => {
  const t = raw.trim()
  if (t.length >= 2 && ((t.startsWith('"') && t.endsWith('"')) || (t.startsWith("'") && t.endsWith("'")))) {
    return t.slice(1, -1)
  }
  return t
}

/** Стрип export-обвязки ядра (паритет cli strip_export_wrapper / wiki_ops
 *  _strip_export_wrapper): frontmatter-блок и ведущая title-H1-строка. */
export function stripExportWrapper(md: string): string {
  const lines = md.split("\n")
  let i = 0
  if (lines[0]?.trim() === "---") {
    const close = lines.findIndex((l, k) => k > 0 && l.trim() === "---")
    i = close === -1 ? 0 : close + 1
  }
  while (i < lines.length && lines[i].trim() === "") i++
  if (lines[i]?.startsWith("# ")) i++
  return lines.slice(i).join("\n").replace(/^\n+/, "")
}

/** Внутренний парсер блока `bot:` (key: value строки; строка `bot:` —
 *  заголовок блока, срезается). Неизвестный ключ — throw (молчаливый пропуск
 *  прячет опечатку владельца — паритет пиннинг-дока). */
function parseBotBlock(docId: string, block: string): Omit<ParsedBotDoc, "slug" | "def"> {
  const spec: Omit<ParsedBotDoc, "slug" | "def"> = { token: "", owner: "", enabled: false }
  const seen = new Set<string>()
  for (const line of block.split("\n")) {
    if (line.trim() === "" || line.trim() === "bot:") continue
    const kv = line.trim().match(/^([a-z]+):\s*(.*)$/)
    if (kv === null) throw new Error(`${docId}: битая строка «${line.trim()}» в блоке bot:`)
    const [, key, raw] = kv
    if (seen.has(key)) throw new Error(`${docId}: дубликат ключа «${key}» в блоке bot:`)
    seen.add(key)
    if (key === "token") spec.token = unquote(raw)
    else if (key === "owner") spec.owner = unquote(raw)
    else if (key === "model") spec.model = unquote(raw)
    else if (key === "workspace") spec.workspace = unquote(raw)
    else if (key === "enabled") {
      if (unquote(raw) !== "true" && unquote(raw) !== "false") throw new Error(`${docId}: enabled — true/false, получено «${raw.trim()}»`)
      spec.enabled = unquote(raw) === "true"
    } else throw new Error(`${docId}: неизвестный ключ «${key}» в блоке bot:`)
  }
  return spec
}

/** Парсинг док-спеки из сырого экспорт-маркдауна ядра. Контракт тела: секция
 *  «## Спека» (первый ```yaml фенс — блок `bot:`), методичка = остальное
 *  тело. slug — тайтл дока после префикса «Бот: ». */
export function parseBotDoc(docId: string, exportMd: string): ParsedBotDoc {
  const lines = exportMd.split("\n")
  if (lines[0]?.trim() !== "---") throw new Error(`${docId}: нет frontmatter (первая строка не «---»)`)
  const close = lines.findIndex((l, i) => i > 0 && l.trim() === "---")
  if (close === -1) throw new Error(`${docId}: frontmatter не закрыт`)
  const titleLine = lines.slice(1, close).find((l) => l.startsWith("title:"))
  const rawTitle = titleLine !== undefined ? unquote(titleLine.slice("title:".length)) : ""
  if (!rawTitle.startsWith(SPEC_PREFIX)) throw new Error(`${docId}: тайтл «${rawTitle}» не начинается с «${SPEC_PREFIX}»`)
  const slug = rawTitle.slice(SPEC_PREFIX.length).trim()
  if (slug === "") throw new Error(`${docId}: пустой слаг (тайтл «Бот: » без имени)`)

  const body = stripExportWrapper(exportMd)
  const bodyLines = body.split("\n")
  const specAt = bodyLines.findIndex((l) => l.trim() === SPEC_HEADING)
  if (specAt === -1) throw new Error(`${docId}: нет секции «${SPEC_HEADING}» в теле`)
  const fenceOpen = bodyLines.findIndex((l, i) => i > specAt && l.trim() === "```yaml")
  if (fenceOpen === -1) throw new Error(`${docId}: нет yaml-фенса в секции «${SPEC_HEADING}»`)
  const relClose = bodyLines.slice(fenceOpen + 1).findIndex((l) => l.trim() === "```")
  if (relClose === -1) throw new Error(`${docId}: yaml-фенс секции «${SPEC_HEADING}» не закрыт`)
  const fenceClose = fenceOpen + 1 + relClose

  const parsed = parseBotBlock(docId, bodyLines.slice(fenceOpen + 1, fenceClose).join("\n"))
  if (parsed.token === "") throw new Error(`${docId}: пустой bot.token`)
  if (parsed.owner === "") throw new Error(`${docId}: пустой bot.owner`)
  if (parsed.model === undefined) throw new Error(`${docId}: пустой bot.model (provider/id[#variant])`)
  if (parsed.workspace === "") throw new Error(`${docId}: пустой bot.workspace`)

  // методичка = тело без диапазона [«## Спека» .. закрывающий фенс]
  const def = [...bodyLines.slice(0, specAt), ...bodyLines.slice(fenceClose + 1)].join("\n").trim()
  if (def === "") throw new Error(`${docId}: методичка пустая — тело дока вне секции «${SPEC_HEADING}» обязательно`)
  return { slug, ...parsed, def }
}

/** «zai-coding-plan/glm-5.3-flash#max» → HubModel (variant опционален).
 *  null — не распарсилось (валидация спеки — fail-loud супервизора). */
export function parseModel(raw: string): HubModel | null {
  const m = raw.match(/^([^/\s]+)\/([^#\s]+)(?:#(\S+))?$/)
  if (m === null) return null
  return { providerID: m[1], id: m[2], ...(m[3] !== undefined ? { variant: m[3] } : {}) }
}

// ——— диф жизненного цикла ———

/** Кэш реестра супервизора: слаг → что было известно на прошлом тике
 *  (переживает рестарт раннера — дети-сироты усыновляются по pid). */
export interface RegistryCacheEntry {
  docId: string
  updatedAt: string
  /** хэш существенного для ребёнка: token+owner+model+workspace+enabled */
  runtimeHash: string
}

/** Действие супервизора по боту на тике. */
export type BotAction =
  | { kind: "spawn"; slug: string }
  | { kind: "restart"; slug: string; reason: string }
  | { kind: "stop"; slug: string }
  | { kind: "def-update"; slug: string }

/** Хэш рантайм-существенного состава спеки (djb2): сравнение между тиками;
 *  криптографии нет и не нужно. */
export function runtimeHashOf(token: string, owner: string, model: { providerID: string; id: string; variant?: string } | undefined, workspace: string | undefined, enabled: boolean): string {
  const modelStr = model === undefined ? "" : `${model.providerID}/${model.id}#${model.variant ?? ""}`
  const raw = `${token}|${owner}|${modelStr}|${workspace ?? ""}|${enabled ? "1" : "0"}`
  let h = 5381
  for (let i = 0; i < raw.length; i++) h = ((h << 5) + h + raw.charCodeAt(i)) | 0
  return (h >>> 0).toString(36)
}

/** Диф тика: свежий скан реестра против кэша + живые дети. Живость ребёнка —
 *  per-бот lock (pid): мёртвый при живой enabled-спеке — spawn (супервизор
 *  поднимает, максимум раз в тик); снесённый док / enabled: off у живого —
 *  stop; смена рантайм-существенного — restart; смена тела (updatedAt) без
 *  рантайма — def-update (ребёнок перечитает agent.md на следующем ходе —
 *  горячая правка методички). */
export function decideActions(
  cache: Record<string, RegistryCacheEntry>,
  scanned: BotObservation[],
  liveChildren: Set<string>,
): BotAction[] {
  const actions: BotAction[] = []
  const scannedSlugs = new Set<string>()
  for (const obs of scanned) {
    scannedSlugs.add(obs.spec.slug)
    const prev = cache[obs.spec.slug]
    const hash = runtimeHashOf(obs.spec.token, obs.spec.owner, obs.spec.model, obs.spec.workspace, obs.spec.enabled)
    const alive = liveChildren.has(obs.spec.slug)
    if (!obs.spec.enabled) {
      if (alive) actions.push({ kind: "stop", slug: obs.spec.slug })
      continue
    }
    if (prev === undefined || !alive) {
      actions.push({ kind: "spawn", slug: obs.spec.slug })
      continue
    }
    if (prev.runtimeHash !== hash) {
      actions.push({ kind: "restart", slug: obs.spec.slug, reason: "сменился токен/owner/модель/workspace/enabled" })
      continue
    }
    if (prev.updatedAt !== obs.updatedAt) actions.push({ kind: "def-update", slug: obs.spec.slug })
  }
  for (const slug of liveChildren) {
    if (!scannedSlugs.has(slug)) actions.push({ kind: "stop", slug })
  }
  return actions
}

// ——— seed-контракт: directory материализации дефа ———

/** Directory живой (seeded) сессии из ответа GET /session. null — не удалось
 *  прочитать (сессия ещё не создана / сбой) — тогда workspace спеки. */
export function liveSessionDirectory(raw: unknown): string | null {
  if (raw === null || typeof raw !== "object") return null
  const d = (raw as { data?: unknown }).data ?? raw
  if (typeof d !== "object") return null
  const dir = (d as { location?: { directory?: unknown } }).location?.directory
  return typeof dir === "string" && dir.trim() !== "" ? dir : null
}

/** Куда материализовать деф при старте ребёнка с seeded-сессией: directory
 *  живой сессии wins (деф резолвится сервером из location сессии), workspace
 *  спеки — для новых сессий. Расхождение — warning-строка вызывающего. */
export function defTargetDirectory(specWorkspace: string | undefined, seededDirectory: string | null): {
  directory: string
  warning?: string
} {
  if (seededDirectory !== null && specWorkspace !== undefined && seededDirectory !== specWorkspace) {
    return {
      directory: seededDirectory,
      warning: `seeded-сессия живёт в «${seededDirectory}», workspace спеки «${specWorkspace}» — деф материализуется в directory сессии`,
    }
  }
  return { directory: seededDirectory ?? specWorkspace ?? "" }
}
