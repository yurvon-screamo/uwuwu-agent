// goal-lib.ts — чистые функции goal-mode: разбор команд, вердикты, транскрипт.
// Без зависимостей — импортируется и плагином, и тестами.

export interface Goal {
  text: string
  since: number
}

export interface TranscriptMessage {
  type: string
  time?: { created?: number }
  content?: Array<{ type?: string; text?: string }>
}

export type VerdictKind = "done" | "not_done" | "impossible"
export type Verdict = { kind: VerdictKind; detail: string }
export type ParsedCommand = { kind: "goal"; text: string } | { kind: "stop" } | null

/** Разбор команды постановки/снятия цели из текста сообщения. */
export function parseGoalCommand(text: string): ParsedCommand {
  if (/^\s*goal-stop\b/i.test(text)) return { kind: "stop" }
  const m = text.match(/^\s*goal:\s*(.+)/i)
  if (m) {
    const goal = m[1].trim()
    if (goal !== "") return { kind: "goal", text: goal }
  }
  return null
}

/** Разбор ответа судьи. Неразбираемый ответ трактуется как NOT_DONE (fail-safe). */
export function parseVerdict(reply: string): Verdict {
  const m = reply.match(/^\s*(GOAL_DONE|GOAL_NOT_DONE|GOAL_IMPOSSIBLE)\s*:?\s*(.*)/i)
  const detail = (m?.[2] ?? reply).trim().slice(0, 600)
  const kind = m?.[1]?.toUpperCase()
  if (kind === "GOAL_DONE") return { kind: "done", detail }
  if (kind === "GOAL_IMPOSSIBLE") return { kind: "impossible", detail }
  return { kind: "not_done", detail }
}

/** Транскрипт с момента постановки цели, обрезанный с хвоста до cap символов. */
export function renderTranscript(
  messages: TranscriptMessage[],
  since: number,
  cap: number,
): string {
  const lines = messages
    .filter((m) => (m.time?.created ?? 0) >= since && (m.type === "user" || m.type === "assistant"))
    .map((m) => {
      const who = m.type === "user" ? "USER" : "AGENT"
      const t = (m.content ?? [])
        .filter((p) => p?.type === "text" && p?.text)
        .map((p) => p.text ?? "")
        .join("\n")
        .trim()
      return t ? `${who}: ${t}` : ""
    })
    .filter(Boolean)
  const joined = lines.join("\n\n")
  return joined.length > cap ? joined.slice(-cap) : joined
}

/** Промпт судьи: цель + транскрипт + требование доказательств. */
export function judgePrompt(goal: string, transcript: string): string {
  return [
    "Ты — строгий судья достижения цели. Проверь цель по ФАКТАМ: файлы, коммиты, выводы команд (используй инструменты чтения). Пересказу агента не верь — проверяй. Слабые или косвенные улики считай недостаточными. Пользователь ушёл — агент действует автономно. Если цель включает код — убедись, что проведён код-ревью плана и изменений.",
    "",
    `ЦЕЛЬ: ${goal}`,
    "",
    "ПЕРЕПИСКА С МОМЕНТА ПОСТАНОВКИ ЦЕЛИ (USER — пользователь, AGENT — агент):",
    transcript || "(пусто)",
    "",
    "Ответь СТРОГО одной строкой, первый токен — вердикт:",
    "GOAL_DONE: <чем подтверждается достижение>",
    "GOAL_NOT_DONE: <конкретно что не сделано>",
    "GOAL_IMPOSSIBLE: <почему цель недостижима>",
  ].join("\n")
}

/** Последний assistant-текст из списка сообщений сессии (пустая строка если нет). */
export function lastAssistantText(messages: TranscriptMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    if (m.type !== "assistant") continue
    const t = (m.content ?? [])
      .filter((p) => p?.type === "text" && p?.text)
      .map((p) => p.text ?? "")
      .join("\n")
      .trim()
    if (t !== "") return t
  }
  return ""
}

/** Финал судьи = последнее assistant-сообщение его сессии. */
export function lastMessageText(segments: Map<string, string>): string {
  let lastMsg = ""
  for (const key of segments.keys()) if (key.includes(":")) lastMsg = key.split(":")[0]
  if (!lastMsg) return [...segments.values()].join("\n").trim()
  return [...segments.entries()]
    .filter(([key]) => key.startsWith(`${lastMsg}:`))
    .sort((a, b) => Number(a[0].split(":")[1]) - Number(b[0].split(":")[1]))
    .map(([, text]) => text)
    .join("\n")
    .trim()
}
