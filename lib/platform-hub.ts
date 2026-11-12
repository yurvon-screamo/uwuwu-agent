//! Hub-режим TS-демонов: реализации tool-имён lib/platform над uwuwu-hub
//! (issues + Projects v2 + body frontmatter) через gh subprocess.
//! Срез D задачи 20261112155302-ttyp35g: демоны живут на hub при
//! UWUWU_HUB_REPO, SiYuan MCP не используется.
//!
//! docId-схема: `issue:<number>` (тело задачи), `doc:<slug>` (md вики из
//! git-клона), `board:<proj>` / `db:<proj>` (фиктивные док/database-блок
//! доски проекта — канбан-семантика factory/triage/cron сохранена).

import { spawnSync } from "node:child_process"

export function hubConfigured(): boolean {
  return (process.env.UWUWU_HUB_REPO ?? "") !== ""
}

function gh(args: string[], stdinBody?: string): string {
  const r = spawnSync("gh", args, {
    input: stdinBody,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
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

export function hubRepo(): string {
  return process.env.UWUWU_HUB_REPO ?? ""
}

/// Issues репо по label (state=all — доска несёт и Done-строки, паритет
/// SiYuan-доски) + frontmatter-разбор body.
export function issuesForProject(project: string): Array<Record<string, unknown>> {
  const data = ghJson<unknown[]>([
    "api", "-X", "GET",
    `repos/${hubRepo()}/issues`,
    "-F", "state=all", "-F", "per_page=100",
    "-f", `labels=proj:${project}`,
  ])
  const arr = Array.isArray(data) ? data : []
  return arr.filter((i: any) => !i.pull_request) as Array<Record<string, unknown>>
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
  const q1 = `query($n: Int!) { viewer { projectV2(number: $n) { id items(first: 100) { nodes { id content { ... on Issue { number } } } } statusField: field(name: "Status") { ... on ProjectV2SingleSelectField { id options { id name } } } } } }`
  const d = JSON.parse(gh([ "api", "graphql", "--input", "-" ],
    JSON.stringify({ query: q1, variables: { n: projNumber } })))
  const proj = d.data.viewer.projectV2
  const item = proj.items.nodes.find((n: any) => n.content?.number === Number(number))
  const opt = proj.statusField.options.find((o: any) => o.name === status)
  if (!item || !opt) return
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
  const node = d.data.viewer.projectsV2.nodes.find((p: any) => p.title === needle)
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
