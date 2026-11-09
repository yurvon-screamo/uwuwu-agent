// tests/platform-url-cell.test.ts — баг-пин: read_database_cells не читает
// клетки колонок типа url (задача 20261109183658-kh3e7qe, живая проба
// 09.11: доска «Задачи uwuwu» — PR-ключ конвертирован в url, у строк с
// url-only клетками PR.value доходит пустым, а CLI (Rust cell_value,
// siyuan_task.rs: Some("url") => /url/content) тот же PR видит). Следствие
// в фабрике: merged-проба по PR-клетке (prProbeByCell) на живой доске
// недостижим — row.pr всегда "" и путь всегда уходит в фоллбек по ветке.
// Ред-пин на текущем проде: тест красный, пока адаптер не читает url.
import { describe, expect, test } from "bun:test"
import { tool, __test } from "../lib/platform"

const AV_ID = "20260101000000-abcdef"
const BLOCK_ID = "20261101000000-avblk1"
const PR_URL = "https://github.com/yurvon-screamo/uwuwu-agent/pull/198"

/** Скриптованный kernel: sql (avIdOfBlock) + MCP database render со строкой,
 *  у которой PR-клетка — активный тип url (shape живого ядра alpha.5:
 *  {"type":"url","url":{"url":...,"content":...}}), stale text отсутствует. */
function scriptKernel(): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url.includes("/api/query/sql")) {
      return new Response(JSON.stringify({
        code: 0, msg: "",
        data: [{ markdown: `<div data-av-id="${AV_ID}"></div>` }],
      }), { status: 200 })
    }
    if (url.includes("/mcp")) {
      const payload = JSON.parse(String(init?.body ?? "{}")) as {
        method?: string
        params?: { name?: string; arguments?: Record<string, unknown> }
      }
      if (payload.method === "initialize") {
        return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: {} }), {
          status: 200, headers: { "mcp-session-id": "sid" },
        })
      }
      if (payload.method === "notifications/initialized") return new Response("", { status: 200 })
      const args = payload.params?.arguments ?? {}
      let result: unknown
      if (payload.params?.name === "database" && String(args.action ?? "") === "render") {
        result = {
          structuredContent: {
            data: {
              view: {
                columns: [
                  { id: "k-key", name: "Название" },
                  { id: "k-pr", name: "PR" },
                ],
                rowCount: 1,
                rows: [{
                  id: "20261109183658-row",
                  cells: [
                    {
                      value: {
                        keyID: "k-key", type: "block", blockID: "20261109183658-row",
                        block: { id: "doc1", content: "Задача", refSubtype: "d" },
                      },
                    },
                    // как отдаёт живое ядро после конвертации ключа text→url
                    // и записи через task edit --pr (pr_cell_value в CLI):
                    // активный url, stale text.content пуст
                    { value: { keyID: "k-pr", type: "url", url: { url: PR_URL, content: PR_URL } } },
                  ],
                }],
              },
            },
          },
        }
      } else {
        throw new Error(`url-cell mock: неожиданный вызов ${payload.params?.name ?? "?"} ${JSON.stringify(args)}`)
      }
      return new Response(JSON.stringify({ jsonrpc: "2.0", id: 2, result }), { status: 200 })
    }
    throw new Error(`url-cell mock: неожиданный путь ${url}`)
  }) as typeof fetch
}

describe("read_database_cells: колонка типа url (баг-пин)", () => {
  test("url-клетка читается в value — паритет Rust cell_value (/url/content)", async () => {
    __test.expireSession()
    scriptKernel()
    const r = await tool<{
      rows?: Array<{ rowBlockId?: string; cells?: Record<string, { value?: string }> }>
    }>("read_database_cells", { databaseBlockId: BLOCK_ID })
    const row = r.rows?.[0]
    expect(row?.rowBlockId).toBe("20261109183658-row")
    // баг: адаптер матчит только mSelect/text/date — url-клетка доходит ""
    expect(row?.cells?.PR?.value).toBe(PR_URL)
  })
})
