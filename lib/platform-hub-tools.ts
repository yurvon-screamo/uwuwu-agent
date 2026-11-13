//! Hub-реализации tool-имён lib/platform (platform-hub.ts — транспорт и
//! docId-схема). Маппинг семантики доски на issues: по контракту миграции
//! Status/Запланировано — Project v2 + frontmatter, Branch/PR/Сессия dev/
//! Расписание — frontmatter.

import { hubCloneRoot } from "./platform-hub"
import type { GhIssue } from "./platform-hub"
import {
  ghJson,
  hubRepo,
  issueByNumber,
  issuePatch,
  issuesForProject,
  parseDocId,
  parseFrontmatter,
  projectOfBoardDocId,
  projectOfDbId,
  setProjectStatus,
  withFields,
} from "./platform-hub"

export async function hubTool<T = Record<string, unknown>>(
  name: string,
  args: Record<string, unknown>,
): Promise<T> {
  switch (name) {
    case "find_doc_by_title": {
      const title = String(args.title ?? "")
      const matches: Array<{ id: string; inTrash: boolean }> = []
      if (title.startsWith("Задачи ")) {
        matches.push({ id: `board:${title.slice("Задачи ".length)}`, inTrash: false })
      } else {
        const needle = title.toLowerCase()
        for (const issue of issuesForProject("misc").length > 0 ? [] : []) void issue
        for (const issue of allIssues()) {
          if (String(issue.title).toLowerCase() === needle) {
            matches.push({ id: `issue:${issue.number}`, inTrash: false })
          }
        }
        const slug = title.toLowerCase()
        if (mdExists(slug)) matches.push({ id: `doc:${title}`, inTrash: false })
      }
      return { matches } as T
    }
    case "read_doc": {
      const docId = String(args.docId ?? "")
      if (projectOfBoardDocId(docId) || projectOfDbId(docId)) {
        return { blocks: [{ id: docId.replace(/^board:/, "db:"), flavour: "platform:database" }] } as T
      }
      return { blocks: [{ id: docId, flavour: "platform:page" }] } as T
    }
    case "get_doc": {
      const docId = String(args.docId ?? "")
      const proj = projectOfBoardDocId(docId)
      if (proj) return { id: docId, title: `Задачи ${proj}`, box: "hub", hpath: `Задачи ${proj}` } as T
      const parsed = parseDocId(docId)
      if (parsed?.kind === "doc") {
        const mdPath = `${hubCloneRoot()}/docs/${parsed.payload}.md`
        let title = parsed.payload.split("/").pop() ?? ""
        let content = ""
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        return { id: docId, title, box: "hub", hpath: parsed.payload, content } as T
      }
      if (parsed?.kind === "issue") {
        const issue = issueByNumber(parsed.payload)
        return {
          id: docId,
          title: issue ? String(issue.title) : "",
          box: "hub",
          hpath: issue ? String(issue.title) : "",
        } as T
      }
      return { id: docId, title: docId, box: "hub", hpath: docId } as T
    }
    case "read_database_cells": {
      const dbId = String(args.databaseBlockId ?? "")
      const project = projectOfDbId(dbId)
      if (!project) throw new Error(`hub: read_database_cells вне доски (${dbId})`)
      const rows = issuesForProject(project).map((issue) => {
        const body = String(issue.body ?? "")
        const { fields } = parseFrontmatter(body)
        const cell = (v: string) => ({ value: v })
        return {
          rowBlockId: String(issue.number),
          cells: {
            "Название": cell(String(issue.title)),
            "Status": cell(fields.status ?? "Todo"),
            "Тип": cell(githubTypeName(issue, fields.type)),
            "Branch": cell(fields.branch ?? ""),
            "PR": cell(fields.pr ?? ""),
            "Сессия dev": cell(fields["dev-session"] ?? ""),
            "Запланировано": cell(fields.planned ?? ""),
            "Расписание": cell(fields.schedule ?? ""),
            "Autodone": cell(fields.autodone === "true" ? "✓" : ""),
            "Приоритет": cell(fields.priority ?? ""),
          },
        }
      })
      return { rows, rowCount: rows.length, columns: Object.keys(rows[0]?.cells ?? {}) } as T
    }
    case "update_database_row": {
      const rowBlockId = String(args.rowBlockId ?? "")
      const cells = (args.cells ?? {}) as Record<string, { value?: string }>
      const plain: Record<string, string> = {}
      const fm: Record<string, string> = {}
      for (const [name, cell] of Object.entries(cells)) {
        const value = cell?.value ?? ""
        if (name === "Status") {
          plain.status = value
          fm.status = value
        } else if (name === "Branch") fm.branch = value
        else if (name === "PR") fm.pr = value
        else if (name === "Сессия dev") fm["dev-session"] = value
        else if (name === "Запланировано") fm.planned = value
        else if (name === "Расписание") fm.schedule = value
        else if (name === "Тип") fm.type = value
        else if (name === "Autodone") fm.autodone = value === "✓" ? "true" : "false"
        else if (name === "Приоритет") fm.priority = value
      }
      const number = rowBlockId.replace(/^issue:/, "")
      const body = String(issueByNumber(number)?.body ?? "")
      issuePatch(number, { body: withFields(body, fm) })
      if (plain.status) setProjectStatus(projectOf(number), number, plain.status)
      return { skipped: [] } as T
    }
    case "ensure_database_columns":
      return { created: [] } as T
    case "create_comment": {
      const docId = String(args.docId ?? "")
      const parsed = parseDocId(docId)
      if (parsed?.kind !== "issue") throw new Error(`hub: create_comment не на issue (${docId})`)
      const content = String(args.content ?? args.text ?? "")
      ghJson([ "api", "-X", "POST", `repos/${hubRepo()}/issues/${parsed.payload}/comments` ],
        JSON.stringify({ body: content }))
      return {} as T
    }
    case "export_doc_markdown": {
      const docId = String(args.docId ?? "")
      const parsed = parseDocId(docId)
      if (parsed?.kind === "doc") {
        const mdPath = `${hubCloneRoot()}/docs/${parsed.payload}.md`
        let title = parsed.payload.split("/").pop() ?? ""
        let content = ""
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        return { id: docId, title, box: "hub", hpath: parsed.payload, content } as T
      }
      if (parsed?.kind === "issue") {
        const issue = issueByNumber(parsed.payload)
        return issue
          ? { markdown: String(issue.body ?? ""), exists: true } as T
          : { markdown: "", exists: false } as T
      }
      if (parsed?.kind === "doc") {
        const md = mdFromClone(parsed.payload)
        return { markdown: md, exists: md !== "" } as T
      }
      return { markdown: "", exists: false } as T
    }
    case "doc_ids_by_hpath": {
      const path = String(args.path ?? args.hpath ?? "")
      const title = path.split("/").filter(Boolean).pop() ?? ""
      const needle = title.toLowerCase()
      const ids: string[] = []
      for (const issue of allIssues()) {
        if (String(issue.title).toLowerCase() === needle) ids.push(`issue:${issue.number}`)
      }
      if (mdExists(title)) ids.push(`doc:${title}`)
      return { ids } as T
    }
    case "doc_exists": {
      const docId = String(args.docId ?? "")
      const parsed = parseDocId(docId)
      if (parsed?.kind === "issue") return { exists: issueByNumber(parsed.payload) !== null } as T
      if (parsed?.kind === "doc") return { exists: mdExists(parsed.payload) } as T
      return { exists: false } as T
    }
    case "get_doc_attrs": {
      const docId = String(args.docId ?? "")
      const parsed = parseDocId(docId)
      if (parsed?.kind === "doc") {
        const mdPath = `${hubCloneRoot()}/docs/${parsed.payload}.md`
        let title = parsed.payload.split("/").pop() ?? ""
        let content = ""
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        return { id: docId, title, box: "hub", hpath: parsed.payload, content } as T
      }
      if (parsed?.kind === "issue") {
        const body = String(issueByNumber(parsed.payload)?.body ?? "")
        return { ...parseFrontmatter(body).fields } as T
      }
      return {} as T
    }
    case "list_organize_nodes": {
      const nodes: Array<Record<string, string>> = [
        { id: "nb-projects", type: "folder", data: "Projects", parentId: "" },
      ]
      for (const project of projects()) {
        nodes.push({ id: `pf:${project}`, type: "folder", data: project, parentId: "nb-projects" })
        nodes.push({ id: `board:${project}`, type: "doc", data: `board:${project}`, parentId: `pf:${project}` })
      }
      // Вики-узлы: папки/доки docs/Projects и docs/Experience — например
      // пиннинг-док «Node-pinning» в проекте uwuwu (resolvePinningDoc демонов).
      // Папка проекта <proj> в wiki-ветке Projects нужна демону resolvePinningDoc
      // (он ищет папку с data == «<proj>», в ней — док «Node-pinning»).
      const clone = hubCloneRoot()
      const wikiRoots: Array<[string, string]> = [["Projects", "Projects"], ["Experience", "Experience"]]
      for (const [dir, idPrefix] of wikiRoots) {
        const folderId = `wiki:${idPrefix}`
        nodes.push({ id: folderId, type: "folder", data: idPrefix, parentId: "nb-projects" })
        const dirPath = `${clone}/docs/${dir}`
        try {
          for (const entry of require("node:fs").readdirSync(dirPath, { withFileTypes: true })) {
            if (entry.name.startsWith(".")) continue
            if (entry.isDirectory()) {
              nodes.push({ id: `wikifolder:${dir}/${entry.name}`, type: "folder", data: entry.name, parentId: folderId })
            } else if (entry.name.endsWith(".md")) {
              const base = entry.name.replace(/\.md$/, "")
              const docId = `doc:${dir}/${base}`
              nodes.push({ id: docId, type: "doc", data: docId, parentId: folderId })
            }
          }
        } catch {}
      }
      // Проектные папки в wiki-ветке Projects: data == имя проекта — там
      // resolvePinningDoc находит док «Node-pinning»
      for (const project of projects()) {
        const folderId = `wikiproject:${project}`
        nodes.push({ id: folderId, type: "folder", data: project, parentId: "wiki:Projects" })
        const dirPath = `${clone}/docs/Projects/${project}`
        try {
          for (const entry of require("node:fs").readdirSync(dirPath, { withFileTypes: true })) {
            if (!entry.isFile() || !entry.name.endsWith(".md")) continue
            const base = entry.name.replace(/\.md$/, "")
            const docId = `doc:Projects/${project}/${base}`
            // parentId == folderId проекта — resolvePinningDoc фильтрует доки
            // по папке пиннинг-проекта
            nodes.push({ id: docId, type: "doc", data: docId, parentId: folderId })
          }
        } catch {}
      }
      return { nodes } as T
    }
    default:
      throw new Error(`hub-режим: tool «${name}» не поддерживается (hub-маппинг срез D)`)
  }
}

// ——— хелперы ———

function allIssues(): Array<Record<string, unknown>> {
  const out: GhIssue[] = []
  let page = 1
  for (;;) {
    const data = ghJson<GhIssue[]>([
      "api", "-X", "GET", `repos/${hubRepo()}/issues`,
      "-F", "state=all", "-F", "per_page=100", "-F", `page=${page}`,
    ])
    const arr = Array.isArray(data) ? data : []
    out.push(...arr)
    if (arr.length < 100) break
    page += 1
  }
  return out.filter((i) => !i.pull_request) as unknown as Array<Record<string, unknown>>
}

function githubTypeName(issue: unknown, fallback: string | undefined): string {
  const name = (issue as GhIssue).type?.name
  return name ? name.toLowerCase() : (fallback ?? "")
}

function projects(): string[] {
  const repo = hubRepo()
  const data = ghJson<unknown[]>([
    "api", "-X", "GET", `repos/${repo}/labels?per_page=100`,
  ])
  const arr = Array.isArray(data) ? data : []
  return arr
    .map((l) => String((l as { name?: string }).name))
    .filter((n) => n.startsWith("proj:"))
    .map((n) => n.slice("proj:".length))
}
function mdClonePath(slug: string): string {
  const root = process.env.UWUWU_HUB_CLONE_ROOT
    ? process.env.UWUWU_HUB_CLONE_ROOT
    : `${process.env.HOME ?? "."}/.local/share/uwuwu-cli/hub-clone`
  return `${root}/${hubRepo().replace("/", "__")}/docs/${slug.replace(/^\/+/, "")}.md`
}

function mdExists(slug: string): boolean {
  const { statSync } = require("node:fs") as typeof import("node:fs")
  return statSync(mdClonePath(slug)).isFile()
}

function mdFromClone(slug: string): string {
  const { readFileSync } = require("node:fs") as typeof import("node:fs")
  return readFileSync(mdClonePath(slug), "utf8")
}

function projectOf(number: string): string {
  const issue = issueByNumber(number)
  const labels = issue?.labels
  if (Array.isArray(labels)) {
    for (const l of labels as Array<{ name?: string }>) {
      const name = typeof l === "object" && l !== null ? String(l.name) : String(l)
      if (name.startsWith("proj:")) return name.slice("proj:".length)
    }
  }
  return "uwuwu"
}
