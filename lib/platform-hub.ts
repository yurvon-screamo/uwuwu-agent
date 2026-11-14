//! Hub-режим TS-демонов: реализации tool-имён lib/platform над uwuwu-hub
//! (issues + Projects v2 + глобальные issue fields) через gh subprocess.
//! Срез D задачи 20261112155302-ttyp35g: демоны живут на hub при
//! UWUWU_HUB_REPO. Носитель клеток переписан задачей 1087 (ADR-098):
//! клетки — глобальные issue fields (payload issue_field_values), Status —
//! Pv2 single-select; fm-шапка тел снесена.
//!
//! docId-схема: `issue:<number>` (тело задачи), `doc:<slug>` (md вики из
//! git-клона), `board:<proj>` / `db:<proj>` (фиктивные док/database-блок
//! доски проекта — канбан-семантика factory/triage/cron сохранена).

import { spawnSync } from "node:child_process"

export function hubConfigured(): boolean {
  return (process.env.UWUWU_HUB_REPO ?? "") !== ""
}

function gh(args: string[], stdinBody?: string): string {
  const full = stdinBody !== undefined ? [...args, "--input", "-"] : args
  // gh api без --input - игнорирует stdin (см. Rust gh_raw — тот же готч)
  const r = spawnSync("gh", full, {
    input: stdinBody,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: 120_000,
  })
  if (r.status !== 0) {
    throw new Error(`gh ${args.slice(0, 3).join(" ")} упал: ${r.stderr.slice(0, 300)}`)
  }
  return r.stdout
}

export function ghJson<T>(args: string[], stdinBody?: string): T {
  const out = gh(args, stdinBody).trim()
  return (out === "" ? {} : JSON.parse(out)) as T
}

export function hubCloneRoot(): string {
  // база + каталог репо (паритет cli from_env: клон живёт в
  // hub-clone/<owner>__<repo>, голая база не содержит docs/)
  const base = process.env.UWUWU_HUB_CLONE_ROOT
    ? process.env.UWUWU_HUB_CLONE_ROOT
    : `${process.env.HOME ?? "."}/.local/share/uwuwu-cli/hub-clone`
  return `${base}/${hubRepo().replace("/", "__")}`
}

export function hubRepo(): string {
  return process.env.UWUWU_HUB_REPO ?? ""
}

/// Issues репо по label (state=all — доска несёт и Done-строки, паритет
/// канбаны) + frontmatter-разбор body.
export interface GhIssue {
  number: number
  title: string
  body?: string | null
  state: string
  updated_at?: string
  pull_request?: unknown
  labels?: Array<{ name?: string }>
  type?: { name?: string }
}

export function issuesForProject(project: string): Array<GhIssue> {
  // query-string: gh -f/-F форма на GET давала искажённую выборку
  // (живой e2e 13.11). Пагинация полная: uwuwu-доска > 100 items.
  const label = encodeURIComponent(`proj:${project}`)
  const out: GhIssue[] = []
  let page = 1
  for (;;) {
    const data = ghJson<GhIssue[]>([
      "api", `repos/${hubRepo()}/issues?labels=${label}&state=all&per_page=100&page=${page}`,
    ])
    const arr = Array.isArray(data) ? data : []
    out.push(...arr.filter((i) => !i.pull_request))
    if (arr.length < 100) break
    page += 1
  }
  return out
}

export function issueByNumber(number: string): Record<string, unknown> | null {
  try {
    return ghJson([ "api", `repos/${hubRepo()}/issues/${number}` ])
  } catch {
    return null
  }
}

/// Имена глобальных issue fields — канон носителя (ADR-098).
export const FIELD_NAMES = {
  priority: "Приоритет",
  autodone: "Autodone",
  branch: "Branch",
  pr: "PR",
  devSession: "Сессия dev",
  reviewSession: "Сессия ревью",
  testSession: "Сессия тест",
  schedule: "Расписание",
  planned: "Запланировано",
} as const

/// Values payload глобальных полей issue (приходит прямо в GET issues
/// и в list — живая проба #1092/задача 1087).
interface IssueFieldValue {
  issue_field_name?: string
  value?: string | number | null
  single_select_option?: { name?: string }
}

export interface GhIssue {
  number: number
  title: string
  body?: string | null
  state: string
  updated_at?: string
  pull_request?: unknown
  labels?: Array<{ name?: string }>
  type?: { name?: string }
  issue_field_values?: IssueFieldValue[]
}

/// Норм-форма клеток issue: text/date — `value`, single_select — имя опции;
/// отсутствующее/пустое поле в карте нет (пустая клетка).
export function fieldsOf(
  issue: { issue_field_values?: IssueFieldValue[] } | Record<string, unknown>,
): Record<string, string> {
  const out: Record<string, string> = {}
  const values = (issue["issue_field_values"] ?? []) as IssueFieldValue[]
  for (const f of values) {
    const name = f.issue_field_name ?? ""
    if (name === "") continue
    const value = f.single_select_option?.name
      ?? (typeof f.value === "string" ? f.value : "")
    if (value !== "") out[name] = value
  }
  return out
}

/// Status-карта доски: issue number → имя статуса (Pv2-канон, батч-джойн;
/// пагинация обязательна — доска uwuwu >700 items).
export function projectStatuses(project: string): Map<number, string> {
  const projNumber = projectBoardNumber(project)
  const out = new Map<number, string>()
  if (projNumber === null) return out
  let cursor: string | null = null
  for (;;) {
    const d = JSON.parse(gh([ "api", "graphql", "--input", "-" ], JSON.stringify({
      query: `query($n: Int!, $cursor: String) { viewer { projectV2(number: $n) {
        items(first: 100, after: $cursor) {
          pageInfo { hasNextPage endCursor }
          nodes {
            content { ... on Issue { number } }
            fieldValueByName(name: "Status") {
              ... on ProjectV2ItemFieldSingleSelectValue { name }
            }
          }
        } } } }`,
      variables: { n: projNumber, cursor },
    })))
    const items = d.data?.viewer?.projectV2?.items?.nodes ?? []
    for (const it of items) {
      const num = it.content?.number
      const status = it.fieldValueByName?.name
      if (typeof num === "number" && typeof status === "string") out.set(num, status)
    }
    const page = d.data?.viewer?.projectV2?.items?.pageInfo
    if (!page?.hasNextPage || typeof page.endCursor !== "string") break
    cursor = page.endCursor
  }
  return out
}

/// Батч-запись клеток: `null` — очистка (DELETE; 404 на отсутствующем
/// значении — no-op), строка — запись (text/date значение-строкой,
/// single_select — именем опции; контракт живой пробы #1092).
export function setIssueFields(
  number: string,
  fields: Record<string, string | null>,
): void {
  const catalog = ghJson<Array<{ id: number; name: string }>>(["api", "/user/issue-fields"])
  const idOf = (name: string): number => {
    const f = catalog.find((c) => c.name === name)
    if (f === undefined) throw new Error(`глобальное поле «${name}» не найдено`)
    return f.id
  }
  const batch: Array<{ field_id: number; value: string }> = []
  for (const [name, value] of Object.entries(fields)) {
    const fid = idOf(name)
    if (value === null || value === "") {
      // очистка: DELETE; 404 (значения не было) — no-op
      try {
        gh([ "api", "-X", "DELETE", `repos/${hubRepo()}/issues/${number}/issue-field-values/${fid}` ])
      } catch (e) {
        if (!String(e).includes("404")) throw e
      }
      continue
    }
    batch.push({ field_id: fid, value })
  }
  if (batch.length === 0) return
  gh([ "api", "-X", "POST", `repos/${hubRepo()}/issues/${number}/issue-field-values` ],
    JSON.stringify({ issue_field_values: batch }))
}

/// Status (single-select) на item issue — Pv2-канан; мутация уже канон
/// (set-путь не тронут миграцией 1087).
export function setProjectStatus(project: string, number: string, status: string): void {
  const projNumber = projectBoardNumber(project)
  if (projNumber === null) return
  // item-резолв через issue.projectItems (без капа items(first:100) —
  // доска uwuwu >700 items: хвост был слеп для статусов демонов)
  const q1 = `query($n: Int!, $o: String!, $r: String!, $num: Int!) { viewer { projectV2(number: $n) { id statusField: field(name: "Status") { ... on ProjectV2SingleSelectField { id options { id name } } } } } repository(owner: $o, name: $r) { issue(number: $num) { projectItems(first: 10) { nodes { id project { id } } } } } }`
  // owner/repo из hubRepo() (env-переопределение UWUWU_HUB_REPO — песочница/
  // e2e): хардкод уводил статусные запросы демонов в прод-репо
  const [owner, repoName] = hubRepo().split("/")
  const d = JSON.parse(gh([ "api", "graphql", "--input", "-" ],
    JSON.stringify({ query: q1, variables: { n: projNumber, o: owner, r: repoName, num: Number(number) } })))
  interface ProjectV2Node {
    id: string
    statusField: { id: string; options: Array<{ id: string; name: string }> }
  }
  const proj = d.data.viewer.projectV2 as ProjectV2Node
  if (!proj?.id) throw new Error(`Projects v2: проект «${project}» не найден`)
  if (!proj.statusField?.id) throw new Error(`Projects v2: поле Status не найдено в «${project}»`)
  interface IssueItemsNode { id: string; project: { id: string } }
  const items = (d.data as { repository?: { issue?: { projectItems?: { nodes: IssueItemsNode[] } } } }).repository?.issue?.projectItems?.nodes ?? []
  const item = items.find((n) => n.project.id === proj.id)
  const opt = proj.statusField.options.find((o) => o.name === status)
  if (!item) throw new Error(`Projects v2: issue #${number} не в проекте после add-item`)
  if (!opt) throw new Error(`Projects v2: опция Status «${status}» не найдена (канон срез A)`)
  const q2 = `mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) { updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i, fieldId: $f, value: {singleSelectOptionId: $o}}) { clientMutationId } }`
  gh([ "api", "graphql", "--input", "-" ], JSON.stringify({
    query: q2,
    variables: { p: proj.id, i: item.id, f: proj.statusField.id, o: opt.id },
  }))
}

function projectBoardNumber(project: string): number | null {
  const d = JSON.parse(gh([ "api", "graphql", "--input", "-" ], JSON.stringify({
    query: "query { viewer { projectsV2(first: 30) { nodes { number title } } } }",
  })))
  const needle = `Задачи ${project}`
  const nodes = d.data.viewer.projectsV2.nodes as Array<{ number: number; title: string }>
  const node = nodes.find((p) => p.title === needle)
  return node ? node.number : null
}

/// docId → (kind, payload). Неизвестная форма — null (вызывающий честный
/// отказ).
export function parseDocId(docId: string): { kind: string; payload: string } | null {
  const i = docId.indexOf(":")
  if (i === -1) return null
  const kind = docId.slice(0, i)
  return { kind, payload: docId.slice(i + 1) }
}

export function projectOfBoardDocId(docId: string): string | null {
  const m = /^board:(.+)$/.exec(docId)
  return m ? m[1] : null
}

export function projectOfDbId(dbId: string): string | null {
  const m = /^db:(.+)$/.exec(dbId)
  return m ? m[1] : null
}
