// tests/platform-organize.test.ts — контракт синтеза list_organize_nodes.
// Потребители узлов (cron.ts cronBoardTitles, factory/cron boardViaOrganize,
// triage.ts refreshBoards) ожидают афин-шейп: проекты — folder-узлы глубины 1
// (data = тайтл), доски — doc-узлы глубины 2 (data = doc id). SiYuan hpath
// без хвостового слэша: /proj, /proj/Задачи proj.
import { afterEach, describe, expect, test } from "bun:test"
import { tool } from "../lib/platform"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

interface DocRow { box: string; id: string; content: string; hpath: string }

const NOTEBOOKS = [
  { id: "nbProj", name: "Projects", closed: false },
  { id: "nbExp", name: "Experience", closed: false },
]

const DOCS: DocRow[] = [
  { box: "nbProj", id: "docProjUwuwu", content: "uwuwu", hpath: "/uwuwu" },
  { box: "nbProj", id: "docBoardUwuwu", content: "Задачи uwuwu", hpath: "/uwuwu/Задачи uwuwu" },
  { box: "nbProj", id: "docProjOriga", content: "origa", hpath: "/origa" },
  { box: "nbProj", id: "docBoardOriga", content: "Задачи origa", hpath: "/origa/Задачи origa" },
  { box: "nbProj", id: "docDeep", content: "глубина-3", hpath: "/origa/Задачи origa/глубина-3" },
  { box: "nbExp", id: "docAffine", content: "affine", hpath: "/affine" },
]

const titleById = new Map(DOCS.map((d) => [d.id, d.content]))

function mockKernel(): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const body = JSON.parse(String(init?.body ?? "{}")) as { stmt?: string }
    const respond = (data: unknown): Response =>
      new Response(JSON.stringify({ code: 0, msg: "", data }),
        { headers: { "Content-Type": "application/json" } })
    if (url.includes("/api/notebook/lsNotebooks")) return respond({ notebooks: NOTEBOOKS })
    if (url.includes("/api/query/sql")) {
      const stmt = String(body.stmt ?? "")
      const boxMatch = stmt.match(/box='([^']+)'/)
      if (boxMatch) {
        const limit = Number(stmt.match(/limit\s+(\d+)/i)?.[1] ?? Number.MAX_SAFE_INTEGER)
        return respond(DOCS.filter((d) => d.box === boxMatch[1]).slice(0, limit))
      }
      const idMatch = stmt.match(/(?:^|\s)id='([^']+)'/)
      if (idMatch) {
        const doc = DOCS.find((d) => d.id === idMatch[1])
        return respond(doc ? [{ id: doc.id, content: doc.content, updated: "1" }] : [])
      }
      return respond([])
    }
    throw new Error(`mock: неожиданный путь ${url}`)
  }) as typeof fetch
}

interface OrganizeNode {
  id?: string
  parentId?: string | null
  type?: string
  data?: string
}

async function organizeNodes(): Promise<OrganizeNode[]> {
  mockKernel()
  const r = await tool<{ nodes?: OrganizeNode[] }>("list_organize_nodes", {})
  return r.nodes ?? []
}

describe("list_organize_nodes — шейп синтеза", () => {
  test("доки глубины 1 — folder-узлы с data=тайтл", async () => {
    const nodes = await organizeNodes()
    const projects = nodes.filter((n) => n.parentId === "nbProj")
    expect(projects.map((n) => ({ type: n.type, data: n.data })).sort((a, b) =>
      String(a.data).localeCompare(String(b.data)))).toEqual([
      { type: "folder", data: "origa" },
      { type: "folder", data: "uwuwu" },
    ])
  })

  test("доки глубины 2+ — doc-узлы с data=doc id, parentId=родительский узел", async () => {
    const nodes = await organizeNodes()
    const board = nodes.find((n) => n.data === "docBoardUwuwu")
    expect(board).toMatchObject({ type: "doc", parentId: "docProjUwuwu" })
    const deep = nodes.find((n) => n.data === "docDeep")
    expect(deep).toMatchObject({ type: "doc", parentId: "docBoardOriga" })
  })
})

describe("list_organize_nodes — контракты потребителей", () => {
  test("taskBoardTitles: folder-дети Projects дают «Задачи <proj>»", async () => {
    const nodes = await organizeNodes()
    const pid = nodes.find((n) => n.type === "folder" && n.data === "Projects")?.id
    expect(pid).toBe("nbProj")
    const projects = nodes
      .filter((n) => n.type === "folder" && n.parentId === pid)
      .map((n) => n.data as string)
      .sort()
    expect(projects).toEqual(["origa", "uwuwu"])
    expect(projects.map((p) => `Задачи ${p}`)).toEqual(["Задачи origa", "Задачи uwuwu"])
  })

  test("boardViaOrganize: folder data=proj → doc-дети data=docId → тайтл доски", async () => {
    const nodes = await organizeNodes()
    const folder = nodes.find((n) => n.type === "folder" && n.data === "uwuwu")
    expect(folder?.id).toBe("docProjUwuwu")
    const docIds = nodes
      .filter((n) => n.type === "doc" && n.parentId === folder?.id)
      .map((n) => n.data as string)
    const resolved = docIds.find((id) => titleById.get(id) === "Задачи uwuwu")
    expect(resolved).toBe("docBoardUwuwu")
  })

  test("refreshBoards: находит «Задачи …» всех проектов", async () => {
    const nodes = await organizeNodes()
    const boards: { project: string; docId: string }[] = []
    for (const f of nodes.filter((n) => n.type === "folder" && typeof n.data === "string" && n.data !== "")) {
      const docIds = nodes
        .filter((n) => n.type === "doc" && n.parentId === f.id && typeof n.data === "string")
        .map((n) => n.data as string)
      for (const docId of docIds) {
        if (titleById.get(docId)?.startsWith("Задачи ")) {
          boards.push({ project: f.data as string, docId })
          break
        }
      }
    }
    expect(boards).toContainEqual({ project: "uwuwu", docId: "docBoardUwuwu" })
    expect(boards).toContainEqual({ project: "origa", docId: "docBoardOriga" })
  })
})
