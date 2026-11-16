// ci-deploy-contract.test.ts — пин контракта .github/workflows/ci.yml на
// автодеплой uwuwu-server (задача uwuwu#1211) по конвенции «текст воркфлоу
// = контракт» (ADR-097; файл-артефакт tests/ci-postmerge-trigger.test.ts —
// исторический, конвенция жива текстом ADR). Статический тест: файл
// читается локально, сети нет (как dockerfile-cache-mounts.test.ts).
//
// Пинится поведение, а не вкусовщина:
//  - deploy-джоба гейтится полным Rust-контуром (needs [changes, fmt, rust])
//    и триггерится только push'ем мастера при server_deploy=true;
//  - deploy НЕ в needs агрегатора «CI» — деплой пост-мержный сайд-эффект,
//    required-контекст branch protection не должен зависеть от Railway;
//  - токен только из secrets (RAILWAY_TOKEN в код не попадает);
//  - деплой-шаг: railway up --ci из server/ с явными -p/-e/-s (без
//    .railway.json в репе флаги обязательны — Railway CLI Operations).
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

// Путь от lib/ вверх: тест должен работать при любом cwd (bun test lib/
// из корня репо, но и точечный запуск bun test lib/x.test.ts — тоже)
const CI_PATH = join(import.meta.dir, "..", ".github", "workflows", "ci.yml")

/** Блок job-а верхнего уровня: от строки `  <job>:` до следующего job-а
 * (та же глубина отступа) или конца файла. */
function jobBlock(src: string, job: string): string {
  const lines = src.split("\n")
  const header = `  ${job}:`
  const start = lines.indexOf(header)
  if (start === -1) return ""
  const rest = lines.slice(start + 1)
  const end = rest.findIndex((line) => line.length > 0 && !line.startsWith("    ") && line.startsWith("  ") && !line.startsWith("   "))
  return rest.slice(0, end === -1 ? rest.length : end).join("\n")
}

/** value последнего `key: value` (или `- key: value`), найденного в блоке. */
function lastMapping(block: string, key: string): string | null {
  const m = [...block.matchAll(new RegExp(`[- ]*\\b${key}: *(.+)$`, "gm"))]
  return m.length ? m[m.length - 1][1].trim() : null
}

const src = readFileSync(CI_PATH, "utf8")
const changes = jobBlock(src, "changes")
const deploy = jobBlock(src, "deploy")
const aggregator = jobBlock(src, "CI")

describe("ci.yml — контракт автодеплоя uwuwu-server (uwuwu#1211)", () => {
  test("changes публикует output server_deploy — деплой-фильтр живёт в changes", () => {
    // Снос output = deploy-джоба без условия триггера; тест ловит расхождение.
    // `${{ … }}` — литерал GitHub Actions expression (пин воркфлоу), не JS-интерполяция.
    // biome-ignore lint/suspicious/noTemplateCurlyInString: Actions expression — литерал
    expect(changes).toContain("server_deploy: ${{ steps.areas.outputs.server_deploy }}")
  })

  test("дифф-база деплоя — github.event.before с гардом форс-пуша (фоллбек HEAD^)", () => {
    // event.before устойчив к merge-commit/squash/rebase и мульти-коммитным
    // пушам; HEAD^ покрывает только последний коммит — гард обязателен.
    // biome-ignore lint/suspicious/noTemplateCurlyInString: литерал Actions expression.
    expect(changes).toContain("PUSH_BASE: ${{ github.event.before }}")
    expect(changes).toContain('base="HEAD^"')
    expect(changes).toContain("0000000000000000000000000000000000000000")
  })

  test("деплой-фильтр = входы образа server/Dockerfile (COPY-набор), не весь server/**", () => {
    // ops/** в образ не входит — не должен триггерить деплой (развязка с
    // релеем, uwuwu#1202). Пин списка: расширение паттерна = новые деплой-
    // триггеры, сужение = тихий пропуск серверных правок.
    expect(changes).toContain(
      "server/crates/*|server/Cargo.toml|server/Cargo.lock|server/Dockerfile|server/config.example.toml) server_deploy=true",
    )
  })

  test("deploy триггерится только push'ем мастера при server_deploy=true", () => {
    // Точная if-строка: без allowed-skipped веток — на push run_all=1 и
    // fmt/rust исполняются всегда; упавший need скипает джобу каскадом
    // needs (fail-closed), мёртвых веток нет.
    expect(lastMapping(deploy, "if")).toBe(
      "github.event_name == 'push' && needs.changes.outputs.server_deploy == 'true'",
    )
  })

  test("deploy гейтится полным Rust-контуром: needs [changes, fmt, rust]", () => {
    // plugins/qlty — не входы образа и не гейт деплоя; снос любого из
    // трёх needs = деплой мимо cargo-валидации.
    expect(lastMapping(deploy, "needs")).toBe("[changes, fmt, rust]")
  })

  test("deploy НЕ в needs агрегатора CI — required-контекст мержей не зависит от Railway", () => {
    // Агрегатор держит required-чек branch protection: если добавить deploy
    // в needs, красный деплой начнёт блокировать мержи (а skipped-deploy
    // потребует allowed-семантики) — регресс контракта ADR-063.
    expect(lastMapping(aggregator, "needs")).toBe("[changes, fmt, rust, plugins, qlty]")
  })

  test("RAILWAY_TOKEN берётся только из secrets — токен не попадает в код", () => {
    // biome-ignore lint/suspicious/noTemplateCurlyInString: литерал Actions expression.
    expect(deploy).toContain("RAILWAY_TOKEN: ${{ secrets.RAILWAY_TOKEN }}")
  })

  test("деплой-шаг: railway up --ci из server/ с явными -p/-e/-s", () => {
    // Без .railway.json в репо (файла нет) флаги обязательны — иначе
    // «no service linked» или деплой не туда (Railway CLI Operations).
    const step = deploy.split("\n").find((line) => line.includes("railway up --ci --project"))
    expect(step).toBeDefined()
    expect(step).toContain("--project uwuwu")
    expect(step).toContain("--environment production")
    expect(step).toContain("--service uwuwu-server")
    expect(step).toContain("cd server &&")
  })

  test("deploy ждёт результат билда: railway up без -d (sync-наблюдаемость)", () => {
    // С -d джоба зеленела бы сразу после аплоада при красном билде —
    // fail-open наблюдаемость. Пин: только --ci, без --detach.
    const step = deploy.split("\n").find((line) => line.includes("railway up --ci --project"))
    expect(step).toBeDefined()
    expect(step).not.toContain("-d")
    expect(step).not.toContain("--detach")
  })
})
