//! Hub-режим TS-демонов: реализации tool-имён lib/platform над uwuwu-hub
//! (issues + Projects v2 + body frontmatter) через gh subprocess.
//! Срез D задачи 20261112155302-ttyp35g: демоны живут на hub при
//! UWUWU_HUB_REPO.
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

/// Frontmatter issue body: строковые поля (status/branch/pr/dev-session/
/// planned/schedule/custom-agent/autodone) + rest (тело после блока).
export function parseFrontmatter(body: string): {
  fields: Record<string, string>
  rest: string
} {
  const fields: Record<string, string> = {}
  if (body.startsWith("---\n")) {
    const end = body.indexOf("\n---\n", 4)
    if (end !== -1) {
      for (const line of body.slice(4, end).split("\n")) {
        const i = line.indexOf(": ")
        if (i !== -1) fields[line.slice(0, i).trim()] = line.slice(i + 2).trim()
      }
      return { fields, rest: body.slice(end + 5) }
    }
  }
  return { fields, rest: body }
}

export function withFields(body: string, set: Record<string, string>): string {
  const { fields, rest } = parseFrontmatter(body)
  const merged: Array<[string, string]> = []
  for (const [k, v] of Object.entries(fields)) {
    if (!(k in set)) merged.push([k, v])
  }
  for (const [k, v] of Object.entries(set)) merged.push([k, v])
  let out = "---\n"
  for (const [k, v] of merged) out += `${k}: ${v}\n`
  out += `---\n${rest}`
  return out
}

export function issuePatch(number: string, payload: Record<string, unknown>): void {
  gh([ "api", "-X", "PATCH", `repos/${hubRepo()}/issues/${number}` ],
    JSON.stringify(payload))
}

/// Projects v2: Status (single-select) на item issue — канбан владельца
/// ходит с frontmatter'ом (демоны пишут оба носителя).
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
