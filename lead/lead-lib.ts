// lead-lib.ts — чистые функции ночного шедулера uwuwu-lead: разбор времени
// аудита, гейт файра, промпт аудита, extra-валидатор state. Тесты без сети:
// `bun test`.

import type { HubState } from "../lib/hub"

export interface AuditAt {
  hour: number
  minute: number
}

const DEFAULT_AUDIT_AT: AuditAt = { hour: 4, minute: 30 }

/** Разбор UWUWU_LEAD_AUDIT_AT «HH:MM»; null = кривое (вызывающий логирует и
 *  берёт дефолт 04:30 по локалке). */
export function parseAuditAt(raw: string | undefined): AuditAt | null {
  if (raw === undefined || raw.trim() === "") return DEFAULT_AUDIT_AT
  const m = /^(\d{1,2}):(\d{2})$/.exec(raw.trim())
  if (m === null) return null
  const hour = Number(m[1]), minute = Number(m[2])
  if (hour > 23 || minute > 59) return null
  return { hour, minute }
}

/** Локальная дата «YYYY-MM-DD» — ключ «сутки отработаны». */
export function localDateKey(d: Date): string {
  const p = (n: number): string => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

/** Пора ли стрелять: локальное время уже ≥ auditAt сегодня и эти сутки ещё
 *  не отработаны. Пропущенный слот (демон лежал в 04:30) доезжает догоном
 *  при старте — аудит полезнее тишины. */
export function auditDue(now: Date, at: AuditAt, lastFireDate: string | undefined): boolean {
  const minutes = now.getHours() * 60 + now.getMinutes()
  if (minutes < at.hour * 60 + at.minute) return false
  return localDateKey(now) !== lastFireDate
}

/** Промпт ночного аудита — ТОЛЬКО метаданные (окно, перечень работ):
 *  контракт аудита живёт в методичке дефа (lead-agent.md), недоверенные
 *  тексты досок/сессий агент читает сам инструментами. */
export function composeAuditPrompt(dateKey: string): string {
  return [
    "[ночной аудит]",
    `сутки: до ${dateKey}`,
    "Выполни суточный аудит по методичке «Аудит»: доски всех проектов (зависшие статусы, битые строки, расхождения тел и статусов), сессии суток (мёртвые посреди хода), техдолг (NOTICED BUT NOT TOUCHING → задачи Draft), отклонения от целей (goal list), повторяющиеся инциденты.",
    "Тихие сутки — ни одного текстового сообщения. Иначе — компактная сводка в чат.",
  ].join("\n")
}

/** Extra-валидатор state.json Lead (хук validState в lib/hub): расписание
 *  крон-шедулера. Битое поле стрипится — шедулер переиграет сутки. */
export function validLeadExtras(r: Record<string, unknown>, out: HubState): void {
  if (r.cron !== null && typeof r.cron === "object" && !Array.isArray(r.cron)) {
    const c = r.cron as Record<string, unknown>
    if (typeof c.lastFireDate === "string" && /^\d{4}-\d{2}-\d{2}$/.test(c.lastFireDate)) {
      out.cron = { lastFireDate: c.lastFireDate }
    }
  }
}
