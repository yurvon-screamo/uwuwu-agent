// engineer-egui-skill.test.ts — контракт локального скилла
// skills/engineer-egui (uwuwu#1733). Тест читает артефакт глазами агента-
// пользователя: роутер SKILL.md должен привести к правилу под симптом,
// ATTRIBUTION.md — честно назвать источник, примеры — собираться на ванильном
// egui 0.37. Статические проверки не требуют сети (конвенция
// lib/incident-registry-slug.test.ts); живая компиляция — под гвардом
// UWUWU_E2E=1 (конвенция lib/qlty-review.test.ts).
import { describe, expect, test } from "bun:test"
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "..")
const SKILL = join(ROOT, "skills", "engineer-egui")
const read = (p: string) => readFileSync(join(SKILL, p), "utf8")
const flat = (s: string) => s.replace(/\s+/g, " ")
const examples = ["theme_tokens.rs", "responsive_row.rs", "widget_button.rs", "kittest_harness.rs"]

describe("SKILL.md — роутер ведёт агента к правилу", () => {
  test("frontmatter несёт имя скилла и триггеры стека", () => {
    // Arrange
    const skill = read("SKILL.md")
    // Act
    const fm = skill.match(/^---\n([\s\S]*?)\n---\n/)?.[1] ?? ""
    // Assert
    expect(fm).toMatch(/^name:\s*engineer-egui\s*$/m)
    const description = fm.match(/^description:\s*"?(.*?)"?\s*$/m)?.[1] ?? ""
    for (const trigger of ["egui", "eframe", "egui_kittest", "layout", "zoom", "scaling", "kittest"]) {
      expect(description.toLowerCase()).toContain(trigger)
    }
  })

  test("каждая ссылка роутера резолвится в живой файл скилла", () => {
    // Arrange
    const skill = read("SKILL.md")
    // Act
    const refs = new Set([...skill.matchAll(/`((?:references|examples|assets)\/[^`]+)`/g)].map((m) => m[1]))
    // Assert
    expect(refs.size).toBeGreaterThan(0)
    for (const ref of refs) expect(existsSync(join(SKILL, ref))).toBe(true)
  })

  test("симптом зума и узкой панели ведёт к layout-правилу и его примеру", () => {
    // Arrange
    const row = read("SKILL.md").split("\n").find((line) => line.includes("Zoom, DPI, narrow panel"))
    // Act — строка таблицы роутера для этого симптома
    // Assert
    expect(row).toBeDefined()
    expect(row).toContain("references/layout-and-sizing.md")
    expect(row).toContain("examples/responsive_row.rs")
  })

  test("layout-правило объясняет points против pixels и даёт матрицу приёмки", () => {
    // Act
    const body = flat(read("references/layout-and-sizing.md"))
    // Assert
    expect(body).toContain("logical points")
    expect(body).toContain("pixels_per_point")
    expect(body).toContain("190, 300 and 400 points wide at 1, 2 and 3 pixels per point")
  })

  test("layout-правило называет ванильные примитивы с защитой минимальной ширины", () => {
    // Act
    const body = read("references/layout-and-sizing.md")
    // Assert
    for (const primitive of ["Sides::new", "shrink_right", "truncate", "StripBuilder", "at_least", "clip(true)"]) {
      expect(body).toContain(primitive)
    }
  })
})

describe("ATTRIBUTION.md — происхождение честно", () => {
  test("каждый файл скилла назван в сквозной ведомости", () => {
    // Arrange
    const ledger = read("ATTRIBUTION.md")
    const walk = (dir: string, prefix = ""): string[] =>
      readdirSync(dir).flatMap((entry) => {
        const rel = prefix ? `${prefix}/${entry}` : entry
        return statSync(join(dir, entry)).isDirectory() ? walk(join(dir, entry), rel) : [rel]
      })
    // Act
    // Ведомость — сам инструмент проверки, а не заимствованный материал.
    const files = walk(SKILL).filter((file) => file !== "ATTRIBUTION.md")
    // Assert
    expect(files.length).toBeGreaterThan(0)
    for (const file of files) expect(ledger, `файл без строки в ведомости: ${file}`).toContain(file)
  })

  test("framed() не выдаётся за метод egui — это локальный хелпер foilcap", () => {
    // Act
    const body = read("references/layout-and-sizing.md")
    // Assert
    expect(body).not.toMatch(/egui::Frame::framed|Frame::framed/)
    expect(flat(body)).toContain("has no `framed` method on `Frame`")
    expect(flat(body)).toContain("This is product code, not an egui API")
    expect(body).toContain("ai-toolbox-ui/src/app.rs")
  })
})

describe("примеры привязаны к ванильному egui 0.37", () => {
  test("каждый пример помечает целевую версию 0.37", () => {
    for (const file of examples) expect(read(`examples/${file}`)).toContain("0.37")
  })

  test("примеры импортируют только egui, egui_extras и egui_kittest", () => {
    for (const file of examples) {
      const uses = [...read(`examples/${file}`).matchAll(/^use\s+([a-zA-Z0-9_]+)/gm)].map((m) => m[1])
      expect(uses.length, `пример без use: ${file}`).toBeGreaterThan(0)
      for (const crate of uses) expect(["egui", "egui_extras", "egui_kittest"]).toContain(crate)
    }
  })

  test("манифест шрифтов валиден, хэши зафиксированы, лицензии на месте", () => {
    // Act
    const manifest = JSON.parse(read("assets/fonts/manifest.json"))
    // Assert
    expect(manifest.schema).toBe("engineer-egui/font-manifest/1")
    expect(manifest.fonts.length).toBeGreaterThan(0)
    for (const font of manifest.fonts) {
      expect(font.sha256).toMatch(/^[0-9a-f]{64}$/)
      expect(existsSync(join(SKILL, "assets/fonts", font.license))).toBe(true)
    }
  })

  test("SVG-свотч несёт токены темы из примера", () => {
    // Act
    const svg = read("assets/palette.svg")
    // Assert
    for (const hex of ["#121316", "#060607", "#f2f1ee", "#2d81ff"]) expect(svg).toContain(hex)
    expect(svg.startsWith("<svg")).toBe(true)
    expect(svg.trimEnd().endsWith("</svg>")).toBe(true)
  })
})

// Живой главный сценарий: собрать примеры скилла на ванильном стеке и прогнать
// kittest-харнесс — то же, что делается руками в scratch. Фикстура строится
// самим тестом из файлов скилла; ничего из ворктри не переиспользуется.
const compileE2E = process.env.UWUWU_E2E === "1"

describe("примеры собираются и тест проходит на vanilla egui 0.37 (UWUWU_E2E=1)", () => {
  test.skipIf(!compileE2E)(
    "cargo check + cargo test на изолированной копии примеров",
    async () => {
      const { spawnSync } = await import("node:child_process")
      const { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } = await import("node:fs")
      const { tmpdir } = await import("node:os")

      // Arrange — фикстура в tmp, не в репо
      const dir = mkdtempSync(join(tmpdir(), "engineer-egui-e2e-"))
      try {
        mkdirSync(join(dir, "src"))
        for (const file of examples) copyFileSync(join(SKILL, "examples", file), join(dir, "src", file))
        writeFileSync(
          join(dir, "Cargo.toml"),
          [
            '[package]',
            'name = "engineer-egui-e2e"',
            'version = "0.1.0"',
            'edition = "2021"',
            "",
            "[dependencies]",
            'egui = { version = "=0.37.0", features = ["default_fonts"] }',
            'egui_extras = "=0.37.0"',
            'egui_kittest = { version = "=0.37.0", default-features = false }',
            "",
          ].join("\n"),
        )
        writeFileSync(
          join(dir, "src/main.rs"),
          [
            "#![allow(dead_code)]",
            "mod theme_tokens;",
            "mod responsive_row;",
            "mod widget_button;",
            "mod kittest_harness;",
            "",
            "fn main() {}",
            "",
            "#[cfg(test)]",
            "mod tests {",
            "    #[test]",
            "    fn named_checkbox_clicks_by_label() {",
            "        super::kittest_harness::checkbox_can_be_clicked_by_label();",
            "    }",
            "}",
            "",
          ].join("\n"),
        )

        // Act
        const cargo = (args: string[]) => {
          const r = spawnSync("cargo", args, { cwd: dir, encoding: "utf8", timeout: 180_000 })
          const out = (r.stdout ?? "") + (r.stderr ?? "")
          expect(r.status, `cargo ${args.join(" ")} failed:\n${out}`).toBe(0)
          return out
        }

        // Assert
        expect(cargo(["check", "--offline"])).toContain("Finished")
        expect(cargo(["test", "--offline"])).toContain("1 passed; 0 failed")
      } finally {
        rmSync(dir, { recursive: true, force: true })
      }
    },
    240_000,
  )
})
