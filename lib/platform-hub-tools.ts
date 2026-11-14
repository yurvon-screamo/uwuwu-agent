//! Hub-реализации tool-имён lib/platform (platform-hub.ts — транспорт и
//! docId-схема). Маппинг семантики доски на issues: по контракту задачи
//! 1087 (ADR-098) клетки — глобальные issue fields, Status — Pv2
//! (projectStatuses-джойн), closed → Done (проекция state).

import { hubCloneRoot } from "./platform-hub"
import type { GhIssue } from "./platform-hub"
import {
  FIELD_NAMES,
  fieldsOf,
  ghJson,
  hubRepo,
  issueByNumber,
  issuesForProject,
  parseDocId,
  projectOfBoardDocId,
  projectOfDbId,
  projectStatuses,
  setIssueFields,
  setProjectStatus,
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
        let exists = false
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          exists = true
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        // контракт resolvePinningDoc: exists + markdown (не content)
        return { id: docId, title, box: "hub", hpath: parsed.payload, exists, markdown: content } as T
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
      const issues = issuesForProject(project)
      // Status — Pv2-канон: батч-джойн статусов доски
      const statuses = projectStatuses(project)
      const rows = issues.map((issue) => {
        const f = fieldsOf(issue)
        const cell = (v: string) => ({ value: v, ...(v !== "" && /^\d{4}-\d{2}-\d{2}$/.test(v) ? { dateMs: dateToMs(v) } : {}) })
        // проекция state: закрытый issue = Done независимо от карточки
        const status = issue.state === "closed"
          ? "Done"
          : (statuses.get(issue.number) ?? "Todo")
        const autodoneRaw = f[FIELD_NAMES.autodone] ?? ""
        return {
          rowBlockId: String(issue.number),
          cells: {
            "Название": cell(String(issue.title)),
            "Status": cell(status),
            "Тип": cell(githubTypeName(issue, f["type"] ?? "")),
            "Branch": cell(f[FIELD_NAMES.branch] ?? ""),
            "PR": cell(f[FIELD_NAMES.pr] ?? ""),
            "Сессия dev": cell(f[FIELD_NAMES.devSession] ?? ""),
            "Сессия ревью": cell(f[FIELD_NAMES.reviewSession] ?? ""),
            "Сессия тест": cell(f[FIELD_NAMES.testSession] ?? ""),
            "Запланировано": cell(f[FIELD_NAMES.planned] ?? ""),
            "Расписание": cell(f[FIELD_NAMES.schedule] ?? ""),
            "Autodone": cell(autodoneRaw === "on" || autodoneRaw === "true" || autodoneRaw === "✓" ? "true" : "false"),
            "Приоритет": cell(f[FIELD_NAMES.priority] ?? ""),
          },
        }
      })
      return { rows, rowCount: rows.length, columns: Object.keys(rows[0]?.cells ?? {}) } as T
    }
    case "update_database_row": {
      const rowBlockId = String(args.rowBlockId ?? "")
      const cells = (args.cells ?? {}) as Record<string, { value?: string }>
      const number = rowBlockId.replace(/^issue:/, "")
      // Status — Pv2-мутация; прочие клетки — глобальные поля (батч).
      // Пустое значение клетки — очистка поля (DELETE).
      let status: string | undefined
      const fields: Record<string, string | null> = {}
      const text = (v: string) => (v === "" ? null : v)
      for (const [name, cell] of Object.entries(cells)) {
        const value = cell?.value ?? ""
        if (name === "Status") {
          status = value
        } else if (name === "Branch") {
          fields[FIELD_NAMES.branch] = text(value)
        } else if (name === "PR") {
          fields[FIELD_NAMES.pr] = text(value)
        } else if (name === "Сессия dev") {
          fields[FIELD_NAMES.devSession] = text(value)
        } else if (name === "Сессия ревью") {
          fields[FIELD_NAMES.reviewSession] = text(value)
        } else if (name === "Сессия тест") {
          fields[FIELD_NAMES.testSession] = text(value)
        } else if (name === "Запланировано") {
          fields[FIELD_NAMES.planned] = text(value)
        } else if (name === "Расписание") {
          fields[FIELD_NAMES.schedule] = text(value)
        } else if (name === "Autodone") {
          fields[FIELD_NAMES.autodone] = value === "✓" || value === "true"
            ? "on"
            : "off"
        } else if (name === "Приоритет") {
          fields[FIELD_NAMES.priority] = text(value)
        }
        // «Тип» — нативный issue type: клетка только для чтения (пишется
        // PATCH'ем type, путь cli edit --type) — молча пропускаем
      }
      if (Object.keys(fields).length > 0) setIssueFields(number, fields)
      if (status !== undefined && status !== "") {
        setProjectStatus(projectOf(number), number, status)
      }
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
        let exists = false
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          exists = true
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        // контракт resolvePinningDoc: exists + markdown (не content)
        return { id: docId, title, box: "hub", hpath: parsed.payload, exists, markdown: content } as T
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
        let exists = false
        try {
          const raw = require("node:fs").readFileSync(mdPath, "utf8")
          content = raw
          exists = true
          const fmTitle = raw.match(/^title:\s*(.+)$/m)
          if (fmTitle) title = fmTitle[1].trim()
        } catch {}
        // контракт resolvePinningDoc: exists + markdown (не content)
        return { id: docId, title, box: "hub", hpath: parsed.payload, exists, markdown: content } as T
      }
      if (parsed?.kind === "issue") {
        return { ...fieldsOf(issueByNumber(parsed.payload) ?? {}) } as T
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
      // Вики-доки проектов — прямые дети pf:<proj>: resolvePinningDoc
      // ищет док «Node-pinning» среди children папки проекта
      for (const project of projects()) {
        const dirPath = `${clone}/docs/Projects/${project}`
        try {
          for (const entry of require("node:fs").readdirSync(dirPath, { withFileTypes: true })) {
            if (!entry.isFile() || !entry.name.endsWith(".md")) continue
            const base = entry.name.replace(/\.md$/, "")
            const docId = `doc:Projects/${project}/${base}`
            nodes.push({ id: docId, type: "doc", data: docId, parentId: `pf:${project}` })
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

/// «Запланировано» YYYY-MM-DD → epoch ms (крон ждёт dateMs клетки).
function dateToMs(date: string): number {
  const d = new Date(`${date}T00:00:00Z`)
  const ms = d.getTime()
  return Number.isNaN(ms) ? 0 : ms
}

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
