// roles-split-contract.test.ts — пин контракта разделения ролей (uwuwu#1630).
// Аналитика формулирует US/UC/NFT-записи в теле задачи, дизайн строит технический
// план против записей, дример переносит записи в спеку по факту AS-IS. Механика
// живёт не в коде, а в методичках и канон-доке, — значит регрессом считается
// правка/удаление самих носителей. Тест статический: читает файлы локально,
// сети нет (конвенция agents-md-user-facing.test.ts).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "..")

function read(rel: string): string {
  return readFileSync(join(ROOT, rel), "utf8")
}

// Переносы строк в markdown не несут смысла — сводим пробелы, чтобы ловить
// формулировку, а не конкретную обёртку строки.
function flat(s: string): string {
  return s.replace(/\s+/g, " ")
}

const template = flat(read("agents/TASK-TEMPLATE.md"))
const analyst = flat(read("agents/analyst.md"))
const reviewer = flat(read("agents/analyst-reviewer.md"))
const canon = flat(read("AGENTS.md"))
const dreamer = flat(read("agents/dreamer.md"))
const all = [template, analyst, reviewer, canon, dreamer].join(" ")

describe("роль аналитика: записи требований в теле задачи (#1630)", () => {
  test("TASK-TEMPLATE несёт форму записей (ID · рамка · источник · класс) и классы (а)/(б)/(в)", () => {
    expect(template).toContain("Requirement records (uwuwu#1630")
    expect(template).toContain("a table in `## Постановка` (local ID · frame · source · class)")
    expect(template).toContain("(а) a verbatim owner quote covering the formulation")
    expect(template).toContain("(б) a link to an existing spec record")
    expect(template).toContain("(в) a canon doc (ADR/README/spec)")
  })

  test("переходная отсечка привязана к мержу PR, а не к плавающей дате", () => {
    expect(template).toContain("before the merge of PR uwuwu-agent#517")
    expect(canon).toContain("before the merge of this rule (PR uwuwu-agent#517)")
  })

  test("direct/fast-track формулируют записи из дословного заказа владельца", () => {
    expect(template).toContain("Direct/fast-track rows: the session formulates records from the owner's verbatim order")
  })

  test("analyst.md: записи в Постановке — обязательная рамка feature, запись без источника — дефект волны", () => {
    expect(analyst).toContain("the requirement records table in `## Постановка`")
    expect(analyst).toContain("A record without a source of any class is a wave defect")
  })

  test("analyst-reviewer.md: запись без источника и feature без таблицы — request_changes", () => {
    expect(reviewer).toContain("Requirement records in the body")
    expect(reviewer).toContain("without a source of any class = request_changes")
    expect(reviewer).toContain("a post-rule feature row without the records table = request_changes")
  })
})

describe("роль дизайна: технический план против записей, трассировка и переходная политика", () => {
  test("канон: требования формулируются в теле, спеку держит TO-BE вне до переноса", () => {
    expect(canon).toContain("formulated as US/UC/NFT records **in the task body**")
    expect(canon).toContain("the dreamer transfers body records into the Design Spec after the AS-IS fact")
  })

  test("трассировка: до переноса локальный ID + номер задачи, влитой код сохраняет локальные ссылки", () => {
    expect(canon).toContain("until transfer: the local ID + the task number")
    expect(canon).toContain("inlaid code keeps its local refs")
  })

  test("строка без таблицы записей (до отсечки) планируется от постановки — не дефект", () => {
    expect(canon).toContain("no records table is not a defect")
  })
})

describe("роль дримера: перенос записей по факту AS-IS", () => {
  test("скан — своя задача, проект — из шапки спавна, не из cwd", () => {
    expect(dreamer).toContain("requirement records transfer (uwuwu#1630")
    expect(dreamer).toContain("the analyzed session's own task only")
    expect(dreamer).toContain("from the session's spawn header in the dump, never from cwd")
  })

  test("якорь AS-IS проверяется по хосту PR: github и Forgejo/Gitea", () => {
    expect(dreamer).toContain("AS-IS anchor")
    expect(dreamer).toContain("gh pr view <url> --json state")
    expect(dreamer).toContain("gt pr list --host <url> --repo <owner>/<repo> --state all")
    expect(dreamer).toContain("never read a failed check as «not merged»")
  })

  test("суждение переноса: новый глобальный ID, «отменено» вместо правки на месте, дедуп", () => {
    expect(dreamer).toContain("a changed formulation → a new record with a new global ID and the old one → status «отменено»")
    expect(dreamer).toContain("an identical formulation exists → skip")
  })

  test("квалификация источника и ключ сопоставления едут в спеке вместе с записью", () => {
    expect(dreamer).toContain("Source qualification travels")
    expect(dreamer).toContain("the spec record's source carries «<project>#<task> <local ID>»")
    expect(dreamer).toContain("uwuwu#1630 US-1")
  })

  test("дисциплина записи: байтовый пре-имидж, sha256, запрет model-retyped копии", () => {
    expect(dreamer).toContain("The pre-image is a byte-exact copy obtained instrumentally")
    expect(dreamer).toContain("sha256-checked against the live blob before the edit")
    expect(dreamer).toContain("a model-retyped copy is not a pre-image")
  })

  test("carve-out поведенческой сверки: записи тела — TO-BE по замыслу, не «поведение без записи»", () => {
    expect(dreamer).toContain("requirement records there are TO-BE by design (uwuwu#1630) and are not a defect")
    expect(canon).toContain("code behavior without a record in the spec or the task body")
  })
})

describe("старая семантика устранена из всех носителей (grep-сверка срезов 4)", () => {
  const stale = [
    "a new/changed requirement → a record in the Design Spec",
    "do NOT add the record yourself",
    "reference the record ID in the spec",
    "code behavior without a spec record",
  ]

  test("ни одна из старых формулировок не вернулась", () => {
    for (const phrase of stale) {
      expect(all).not.toContain(phrase)
    }
  })
})
