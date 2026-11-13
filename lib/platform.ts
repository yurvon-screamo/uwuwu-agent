// lib/platform.ts — фасад платформы uwuwu: всё через hub (репо uwuwu-hub:
// issues = задачи, Projects v2 = канбаны, docs md = вики); контракт
// экспортов сохранён (mcpConfigured/connect/tool/парсеры).
import { hubConfigured } from "./platform-hub"
import { hubTool } from "./platform-hub-tools"

/** Платформа не сконфигурирована/не ответила: семантика «ядро не ответило»
 *  (ADR-033) — демоны ветвятся instanceof, не маскируя отказ фоллбеком. */
export class KernelUnavailableError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "KernelUnavailableError"
  }
}

/** Штатная пауза контура (бывш. «волна деградации ядра»): отдельный класс
 *  поверх KernelUnavailableError — крон не считает такой тик в
 *  consecutiveFails, рестарт не нужен; наследник матчится родителем. */
export class KernelWaveError extends KernelUnavailableError {
  constructor(message: string) {
    super(message)
    this.name = "KernelWaveError"
  }
}

/** Платформа сконфигурирована: UWUWU_HUB_REPO задан (CLI ставит дефолт
 *  установки; явный export — для нестандартного репо). */
export const mcpConfigured = hubConfigured

/** Совместимость: у hub-фасада нет фазы подключения (git-клон ленивый
 *  внутри операций). Оставлено для вызовов factory. */
export async function connect(): Promise<void> {}

/** JSON.parse с честной ошибкой: HTTP-статус и сниппет тела вместо голого SyntaxError,
 *  маскирующего причину (прод-кейс 04.11: «Unexpected identifier "session"» = 404 ядра).
 *  Экспорт: тот же контракт у транспорта opencode API в cron.ts. */
export function parseJsonContract(text: string, context: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    throw new Error(`${context}: не-JSON: ${text.slice(0, 120)}`)
  }
}

export interface ParsedComment {
  id: string
  text: string
  userId: string
  replies: { id: string; text: string; userId: string }[]
}

const TOP_COMMENT = /^- \*\*(.+?)\*\* \(([^)]*)\): (.*)$/
const TOP_COMMENT_DATED = /^- \*\*(.+?)\*\* \((\d{4}-\d{2}-\d{2}[^)]*)\): (.*)$/
const REPLY_COMMENT = /^ {2}- \*\*(.+?)\*\* \(([^)]*)\): (.*)$/
const REPLY_COMMENT_DATED = /^ {2}- \*\*(.+?)\*\* \((\d{4}-\d{2}-\d{2}[^)]*)\): (.*)$/

const commentHead = (m: RegExpMatchArray): string => `${m[1]} (${m[2]}): ${m[3]}`

/** FNV-1a (32-bit) → 8 hex-символов: контентный хэш стабильных id комментов. */
const fnv1aHex = (input: string): string => {
  let hash = 0x811c9dc5
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193) >>> 0
  }
  return hash.toString(16).padStart(8, "0")
}

/** Стабильный id коммента: хэш (автор ‖ финальный текст с датой и
 *  континуациями). Позиция в доке в id не входит — снос/сдвиг чужих
 *  комментов id не меняет, lastCommentId треков не подменяется. */
const stableCommentId = (kind: "c" | "r", docId: string, userId: string, text: string): string =>
  `${kind}${docId}-${fnv1aHex(`${userId}\u0000${text}`)}`

/** Разбор коммент-записей markdown-экспорта тела. Два источника:
 *  (а) секция «## Комменты» — историческая конвенция;
 *  (б) трейлинг-хвост после последней ## — легаси-эпоха аппендил комменты
 *  в конец дока, в телах с секциями после «## Комменты» (слияния дефектов —
 *  регулярный паттерн владельца) свежие записи живут хвостом; без (б) comment-
 *  канал слеп к ним — юзерский ответ не будит ни одним каналом (High-1 ревью
 *  раунда 3). Хвост строгим матчем с датой — легитимные списки не комменты.
 *  Продолжения блока (абзацы тела пункта — вердикты многострочные) конкатенируются
 *  в text — будильник цитирует коммент целиком. id двухпроходные: скан
 *  собирает финальные тексты, после скана id = контентный хэш — позиция
 *  в доке не участвует, снос старого коммента сверху id остальных не
 *  двигает (позиционные сдвигались и подменяли lastCommentId треков).
 *  Replies несут id/userId: юзерский тред-ответ детектится, агентский
 *  срезается own-фильтром. */
export function parseCommentsMarkdown(md: string, docId: string): ParsedComment[] {
  const lines = md.split("\n")
  const comments: ParsedComment[] = []
  let inSection = false
  let lastHeadIdx = -1
  let lastHeadIsComments = false
  const appendContinuation = (line: string): void => {
    if (comments.length === 0 || line.trim() === "") return
    const last = comments[comments.length - 1]
    last.text = `${last.text}\n${line}`
  }
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    if (line.startsWith("## ")) {
      lastHeadIdx = i
      lastHeadIsComments = /^## Комменты\s*$/.test(line)
      inSection = lastHeadIsComments
      continue
    }
    if (!inSection) continue
    const top = line.match(TOP_COMMENT)
    if (top) {
      comments.push({ id: "", text: commentHead(top), userId: top[1], replies: [] })
      continue
    }
    const rep = line.match(REPLY_COMMENT)
    if (rep && comments.length > 0) {
      comments[comments.length - 1].replies.push({ id: "", text: commentHead(rep), userId: rep[1] })
      continue
    }
    appendContinuation(line)
  }
  if (!lastHeadIsComments) {
    for (let i = lastHeadIdx + 1; i < lines.length; i++) {
      const line = lines[i]
      const top = line.match(TOP_COMMENT_DATED)
      if (top) {
        comments.push({ id: "", text: commentHead(top), userId: top[1], replies: [] })
        continue
      }
      const rep = line.match(REPLY_COMMENT_DATED)
      if (rep && comments.length > 0) {
        comments[comments.length - 1].replies.push({ id: "", text: commentHead(rep), userId: rep[1] })
        continue
      }
      if (i > lastHeadIdx) appendContinuation(line)
    }
  }
  for (const c of comments) {
    c.id = stableCommentId("c", docId, c.userId, c.text)
    for (const r of c.replies) r.id = stableCommentId("r", docId, r.userId, r.text)
  }
  return comments
}

function commentToSnapshot(text: string): Record<string, unknown> {
  // шейп, который walk-ает snapshotText() фабрики
  return {
    snapshot: {
      blocks: {
        id: "page", type: "block",
        children: [{ id: "note", type: "block", children: [{ id: "p", type: "block", props: { text: { delta: [{ insert: text }] } } }] }],
      },
    },
  }
}


/** Единая точка входа инструментов платформы: hub-реализации (gh + Projects
 *  v2 + git-клон вики). Не-hub окружение — громкий отказ с подсказкой. */
export async function tool<T = Record<string, unknown>>(name: string, args: Record<string, unknown>): Promise<T> {
  if (!hubConfigured()) {
    throw new KernelUnavailableError(
      "платформа не сконфигурирована: UWUWU_HUB_REPO не задан (дефолт установки применяется только через uwuwu-cli hub_repo_default)")
  }
  return hubTool<T>(name, args)
}
