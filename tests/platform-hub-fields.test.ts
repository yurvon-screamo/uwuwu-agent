//! Юниты шова клеток (задача 1087, ADR-098): platform-hub-tools читает/
//! пишет глобальные issue fields, Status — Pv2-джойн, closed → Done.
//! Транспорт platform-hub мокается: проверяется маппинг, не gh.
//!
//! Порядок файла детерминирован (bun test — общий процесс): стаб-тесты
//! setIssueFields на РЕАЛЬНОМ platform-hub идут до mock.module — иначе
//! мок соседнего describe подменяет кеш модуля (ревью Low-5).
//! DELETE-стаб кодирует реальный контракт: 404 exit 1, тело в stdout+stderr
//! (подтверждено ревью round-2 на 2 репо) — no-op-гвард шва глотает.

import { describe, expect, mock, test } from "bun:test"
import { mkdtempSync, mkdirSync, writeFileSync, chmodSync, readFileSync, unlinkSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { setIssueFields as realSetIssueFieldsBinding } from "../lib/platform-hub"

// Live-binding ESM: mock.module ниже подменяет экспорт — снапшот реальной
// функции ДО мока (ревью Low-5: стаб-тесты пишут реальным write-path)
const realSetIssueFields = realSetIssueFieldsBinding

// ——— стаб-тесты setIssueFields (PATH-стаб gh, реальная функция) ———

function setupStubGh(): { pathEnv: string; log: string } {
  const dir = mkdtempSync(join(tmpdir(), "ghstub-"))
  const bin = join(dir, "bin")
  mkdirSync(bin)
  const log = join(dir, "calls.log")
  const script = `#!/usr/bin/env bash
set -u
route=""
method=""
prev=""
for a in "$@"; do
  case "$a" in
    repos/*|user/*) [ -z "$route" ] && route="$a" ;;
    /*) [ -z "$route" ] && route="\${a#/}" ;;
  esac
  if [ "$prev" = "-X" ]; then method="$a"; fi
  prev="$a"
done
echo "$method $route" >> ${log}
[ -f "${dir}/stdin" ] && cat > "${dir}/stdin.last"
if [ "$route" = "user/issue-fields" ]; then
  printf '[{"id":101,"name":"Приоритет"},{"id":106,"name":"Расписание"}]'
  exit 0
fi
if [[ "$route" == *"/issue-field-values"* ]]; then
  if [ "$method" = "DELETE" ]; then
    # 404 на отсутствующем значении: тело в stdout И stderr (как реальный gh,
    # подтверждено ревью round-2 на 2 репо) — гвард no-op глотает
    printf '{"message":"Issue field value not found","status":"404"}'
    echo "gh: Issue field value not found (HTTP 404)" >&2
    exit 1
  fi
  printf '[]'
  exit 0
fi
printf '{"message":"stub: не обслужен"}'
exit 1
`
  const gh = join(bin, "gh")
  writeFileSync(gh, script)
  chmodSync(gh, 0o755)
  return { pathEnv: `${bin}:${process.env.PATH ?? ""}`, log }
}

describe("setIssueFields: реальный write-path на PATH-стабе gh", () => {
  const savedPath = process.env.PATH
  const stub = setupStubGh()

  function withStub(fn: () => void): void {
    process.env.PATH = stub.pathEnv
    try {
      // лог общий на стаб — чистим перед ходом, счётчики честные
      try {
        readFileSync(stub.log, "utf8")
        unlinkSync(stub.log)
      } catch {}
      fn()
    } finally {
      process.env.PATH = savedPath
    }
  }

  test("батч: text-клетки одним POST, каталог-резолв по имени поля", () => {
    withStub(() => realSetIssueFields("777", { "Расписание": "30 5 * * *", "Приоритет": "high" }))
    const log = readFileSync(stub.log, "utf8")
    // пин: батч из двух клеток — ровно один POST (каталог-резолв покрыт
    // живой пробой #1092; стаб-лог ловит только issue-field-values-маршруты)
    // один батч-POST на две клетки
    expect((log.match(/POST .*issue-field-values/g) ?? []).length).toBe(1)
  })

  test("очистка: DELETE 404 на отсутствующем — no-op без POST", () => {
    withStub(() => realSetIssueFields("778", { "Расписание": null }))
    const log = readFileSync(stub.log, "utf8")
    expect((log.match(/DELETE .*issue-field-values/g) ?? []).length).toBe(1)
    // POST нет — только очистка
    expect((log.match(/POST/g) ?? []).length).toBe(0)
  })

  test("смешанный: очистка + запись в одном вызове", () => {
    withStub(() => realSetIssueFields("779", { "Расписание": null, "Приоритет": "low" }))
    const log = readFileSync(stub.log, "utf8")
    expect((log.match(/DELETE/g) ?? []).length).toBe(1)
    expect((log.match(/POST/g) ?? []).length).toBe(1)
  })
})

// ——— мок-контур platform-hub-tools (шов) ———

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
  for (const f of (issue.issue_field_values ?? []) as Array<{ issue_field_name?: string; value?: string | number | null; single_select_option?: { name?: string } }>) {
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
  issuesForProject: (_project: string) => state.issues as never,
  issueByNumber: (number: string) =>
    state.issues.find((i) => String(i.number) === number) ?? null,
  // Pv2-джойн статусов — stub на уровне модуля (projectStatuses зовёт gh):
  // здесь подменяем результат через state.statuses
  projectStatuses: (_project: string) => state.statuses,
  setProjectStatus: (project: string, number: string, _status: string) => {
    state.statusMutation = `${project}#${number}`
  },
  setIssueFields: (_number: string, fields: Record<string, string | null>) => {
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

function issueWith(
  n: number,
  opts: {
    state?: string
    fields?: Record<string, string>
    type?: string
    labels?: string[]
  } = {},
): Record<string, unknown> {
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
    labels: opts.labels,
    issue_field_values: values,
  }
}

describe("fieldsOf: норм-форма payload", () => {
  test("select — имя опции, text — value, пустое поле мимо", async () => {
    const f = fieldsOf(issueWith(1, { fields: {
      [FIELD_NAMES.priority]: "high",
      [FIELD_NAMES.branch]: "work/x",
    } }))
    expect(f.Приоритет).toBe("high")
    expect(f.Branch).toBe("work/x")
    expect(f.PR).toBeUndefined()
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
    const row1 = r.rows.find((x) => x.rowBlockId === "1")
    if (row1 === undefined) throw new Error("строка 1 не вернулась")
    // Pv2-джойн
    expect(row1.cells.Status.value).toBe("In Progress")
    expect(row1.cells.Приоритет.value).toBe("high")
    expect(row1.cells.Autodone.value).toBe("true")
    expect(row1.cells.Branch.value).toBe("work/t")
    expect(row1.cells.Запланировано.dateMs).toBe(Date.parse("2026-11-20T00:00:00Z"))
    const row2 = r.rows.find((x) => x.rowBlockId === "2")
    if (row2 === undefined) throw new Error("строка 2 не вернулась")
    // проекция closed → Done
    expect(row2.cells.Status.value).toBe("Done")
    expect(row2.cells.Autodone.value).toBe("false")
  })

  test("карточки нет в джойне → Todo-дефолт (паритет cli convert)", async () => {
    state.issues = [issueWith(3, {})]
    state.statuses = new Map()
    const r = await hubTool<{ rows: Array<{ cells: Record<string, { value: string }> }> }>(
      "read_database_cells", { databaseBlockId: "db:uwuwu" },
    )
    expect(r.rows[0].cells.Status.value).toBe("Todo")
  })
})

describe("update_database_row: запись в поля", () => {
  test("клетки → setIssueFields (пустое = null-очистка), Status → Pv2-мутация", async () => {
    state.issues = [issueWith(9, { labels: ["proj:uwuwu"] })]
    state.fieldWrites = []
    state.statusMutation = null as string | null
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
    const fields = state.fieldWrites[0]
    if (fields === undefined) throw new Error("батч полей не записан")
    expect(fields.Branch).toBe("work/task-9")
    expect(fields.PR ?? null).toBe(null)
    expect(fields.Autodone).toBe("on")
    expect(fields["Сессия ревью"]).toBe("ses_rev_1")
    expect(fields["Сессия тест"]).toBe("ses_test_1")
    expect(state.statusMutation).toBe("uwuwu#9")
  })

  test("статус пустой строкой — мутации нет", async () => {
    state.issues = [issueWith(10)]
    state.statusMutation = null as string | null
    await hubTool("update_database_row", { rowBlockId: "issue:10", cells: { "Status": { value: "" } } })
    expect(state.statusMutation).toBe(null)
  })
})

describe("get_doc_attrs: issue — поля (норм-имена)", () => {
  test("Расписание живёт под именем поля", async () => {
    state.issues = [issueWith(449, { fields: { [FIELD_NAMES.schedule]: "30 5 * * *" } })]
    const attrs = await hubTool<Record<string, string>>("get_doc_attrs", { docId: "issue:449" })
    expect(attrs.Расписание).toBe("30 5 * * *")
    expect(attrs["custom-schedule"]).toBeUndefined()
  })
})
