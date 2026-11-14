//! Юниты шова клеток (задача 1087, ADR-098): platform-hub-tools читает/
//! пишет глобальные issue fields, Status — Pv2-джойн, closed → Done.
//! Транспорт platform-hub мокается: проверяется маппинг, не gh.

import { describe, expect, mock, test } from "bun:test"

const fieldsCatalog = [
  { id: 101, name: "Приоритет" },
  { id: 102, name: "Autodone" },
  { id: 103, name: "Branch" },
  { id: 104, name: "PR" },
  { id: 105, name: "Сессия dev" },
  { id: 108, name: "Сессия ревью" },
  { id: 109, name: "Сессия тест" },
  { id: 106, name: "Расписание" },
  { id: 107, name: "Запланировано" },
]

const state = {
  issues: [] as Array<Record<string, unknown>>,
  statuses: new Map<number, string>(),
  fieldWrites: [] as Array<Record<string, string | null>>,
  graphql: [] as string[],
  statusMutation: null as string | null,
}

const fieldsOfImpl = (issue: Record<string, unknown>): Record<string, string> => {
  const out: Record<string, string> = {}
  for (const f of (issue["issue_field_values"] ?? []) as Array<{ issue_field_name?: string; value?: string | number | null; single_select_option?: { name?: string } }>) {
    const name = f.issue_field_name ?? ""
    if (name === "") continue
    const value = f.single_select_option?.name ?? (typeof f.value === "string" ? f.value : "")
    if (value !== "") out[name] = value
  }
  return out
}

mock.module("../lib/platform-hub", () => ({
  FIELD_NAMES: {
    priority: "Приоритет",
    autodone: "Autodone",
    branch: "Branch",
    pr: "PR",
    devSession: "Сессия dev",
    reviewSession: "Сессия ревью",
    testSession: "Сессия тест",
    schedule: "Расписание",
    planned: "Запланировано",
  },
  hubRepo: () => "owner/hub",
  hubCloneRoot: () => "/tmp/hub-clone",
  ghJson: (args: string[]) => {
    if (args.includes("/user/issue-fields")) return fieldsCatalog
    throw new Error(`stub: необработанный ghJson ${args.join(" ")}`)
  },
  gh: (args: string[], stdinBody?: string) => {
    if (args.includes("graphql")) {
      const payload = JSON.parse(stdinBody ?? "{}")
      state.graphql.push(payload.query)
      if (payload.query.includes("projectStatusesStub")) {
        return JSON.stringify({ data: { viewer: { projectV2: { items: { pageInfo: { hasNextPage: false, endCursor: null }, nodes: [] } } } } })
      }
      return JSON.stringify({ data: {} })
    }
    if (args.includes("-X") && args.includes("POST") && args.join(" ").includes("issue-field-values")) {
      const payload = JSON.parse(stdinBody ?? "{}")
      state.fieldWrites.push(payload.issue_field_values)
      return "[]"
    }
    if (args.includes("DELETE")) return ""
    throw new Error(`stub: необработанный gh ${args.join(" ")}`)
  },
  issuesForProject: (project: string) => state.issues as never,
  issueByNumber: (number: string) =>
    state.issues.find((i) => String(i["number"]) === number) ?? null,
  // Pv2-джойн статусов — stub на уровне модуля (projectStatuses зовёт gh):
  // здесь подменяем результат через state.statuses
  projectStatuses: (project: string) => state.statuses,
  setProjectStatus: (project: string, number: string, status: string) => {
    state.statusMutation = `${project}#${number}→${status}`
  },
  setIssueFields: (number: string, fields: Record<string, string | null>) => {
    state.fieldWrites.push(fields)
  },
  fieldsOf: fieldsOfImpl,
  parseDocId: (docId: string) => {
    const i = docId.indexOf(":")
    return i === -1 ? null : { kind: docId.slice(0, i), payload: docId.slice(i + 1) }
  },
  projectOfBoardDocId: () => null,
  projectOfDbId: (dbId: string) => dbId.startsWith("db:") ? dbId.slice(3) : null,
}))

const { hubTool } = await import("../lib/platform-hub-tools")
const { fieldsOf } = await import("../lib/platform-hub").then((m) => ({ fieldsOf: (m as { fieldsOf?: typeof fieldsOfImpl }).fieldsOf ?? fieldsOfImpl }))
const { FIELD_NAMES } = await import("../lib/platform-hub")

function issueWith(n: number, opts: {
  state?: string
  fields?: Record<string, string>
  type?: string
} = {}): Record<string, unknown> {
  const values = Object.entries(opts.fields ?? {}).map(([name, value]) => ({
    issue_field_name: name,
    single_select_option: ["Приоритет", "Autodone"].includes(name) ? { name: value } : undefined,
    value: ["Приоритет", "Autodone"].includes(name) ? undefined : value,
  }))
  return {
    number: n,
    title: `задача ${n}`,
    state: opts.state ?? "open",
    body: "чистый контент",
    type: opts.type ? { name: opts.type } : undefined,
    issue_field_values: values,
  }
}

describe("fieldsOf: норм-форма payload", () => {
  test("select — имя опции, text — value, пустое поле мимо", async () => {
    const f = fieldsOf(issueWith(1, { fields: {
      [FIELD_NAMES.priority]: "high",
      [FIELD_NAMES.branch]: "work/x",
    } }))
    expect(f["Приоритет"]).toBe("high")
    expect(f["Branch"]).toBe("work/x")
    expect(f["PR"]).toBeUndefined()
  })
})

describe("read_database_cells: клетки из полей", () => {
  test("Status — Pv2-джойн, closed → Done, Autodone норм-форма, dateMs", async () => {
    state.issues = [
      issueWith(1, { fields: {
        [FIELD_NAMES.priority]: "high",
        [FIELD_NAMES.autodone]: "on",
        [FIELD_NAMES.planned]: "2026-11-20",
        [FIELD_NAMES.branch]: "work/t",
      } }),
      issueWith(2, { state: "closed", fields: {} }),
    ]
    state.statuses = new Map([[1, "In Progress"]])
    const r = await hubTool<{ rows: Array<{ rowBlockId: string; cells: Record<string, { value?: string; dateMs?: number }> }> }>(
      "read_database_cells", { databaseBlockId: "db:uwuwu" },
    )
    const row1 = r.rows.find((x) => x.rowBlockId === "1")!
    expect(row1.cells["Status"].value).toBe("In Progress", "Pv2-джойн")
    expect(row1.cells["Приоритет"].value).toBe("high")
    expect(row1.cells["Autodone"].value).toBe("true")
    expect(row1.cells["Branch"].value).toBe("work/t")
    expect(row1.cells["Запланировано"].dateMs).toBe(Date.parse("2026-11-20T00:00:00Z"))
    const row2 = r.rows.find((x) => x.rowBlockId === "2")!
    expect(row2.cells["Status"].value).toBe("Done", "closed → Done")
    expect(row2.cells["Autodone"].value).toBe("false")
  })

  test("карточки нет в джойне → Todo-дефолт (паритет cli convert)", async () => {
    state.issues = [issueWith(3, {})]
    state.statuses = new Map()
    const r = await hubTool<{ rows: Array<{ cells: Record<string, { value: string }> }> }>(
      "read_database_cells", { databaseBlockId: "db:uwuwu" },
    )
    expect(r.rows[0].cells["Status"].value).toBe("Todo")
  })
})

describe("update_database_row: запись в поля", () => {
  test("клетки → setIssueFields (пустое = null-очистка), Status → Pv2-мутация", async () => {
    state.issues = [issueWith(9, { labels: ["proj:uwuwu"] })]
    state.fieldWrites = []
    state.statusMutation = null
    await hubTool("update_database_row", {
      rowBlockId: "issue:9",
      cells: {
        "Branch": { value: "work/task-9" },
        "PR": { value: "" },
        "Autodone": { value: "✓" },
        "Сессия ревью": { value: "ses_rev_1" },
        "Сессия тест": { value: "ses_test_1" },
        "Status": { value: "Ревью результата" },
      },
    })
    expect(state.fieldWrites.length).toBe(1)
    const fields = state.fieldWrites[0]!
    expect(fields["Branch"]).toBe("work/task-9")
    expect(fields["PR"]).toBeNull()
    expect(fields["Autodone"]).toBe("on")
    expect(fields["Сессия ревью"]).toBe("ses_rev_1")
    expect(fields["Сессия тест"]).toBe("ses_test_1")
    expect(state.statusMutation).toBe("uwuwu#9→Ревью результата")
  })

  test("статус пустой строкой — мутации нет", async () => {
    state.issues = [issueWith(10)]
    state.statusMutation = null
    await hubTool("update_database_row", { rowBlockId: "issue:10", cells: { "Status": { value: "" } } })
    expect(state.statusMutation).toBeNull()
  })
})

describe("get_doc_attrs: issue — поля (норм-имена)", () => {
  test("Расписание живёт под именем поля", async () => {
    state.issues = [issueWith(449, { fields: { [FIELD_NAMES.schedule]: "30 5 * * *" } })]
    const attrs = await hubTool<Record<string, string>>("get_doc_attrs", { docId: "issue:449" })
    expect(attrs["Расписание"]).toBe("30 5 * * *")
    expect(attrs["custom-schedule"]).toBeUndefined()
  })
})
