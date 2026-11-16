// dockerfile-cache-mounts.test.ts — пин контракта server/Dockerfile на
// cache mounts (задача uwuwu#1162). Билдер Railway отвергает безымянные
// cache-маунты как невалидный Dockerfile ("missing an id argument" —
// билд 275d5fd5 FAILED 15.11, PR #335): явные id — не стилистика, а
// валидность. Второй пин — cp-трюк: содержимое cache mount не попадает
// в слои образа, поэтому бинарь копируется в обычный слой до конца RUN.
// Статический тест (файл читается локально, сети нет); поведение сборки
// доказывается прод-деплоями Railway и локальным podman-прогоном.
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

// Путь от lib/ вверх: тест должен работать при любом cwd (bun test lib/
// из корня репо, но и точечный запуск bun test lib/x.test.ts — тоже)
const DOCKERFILE_PATH = join(import.meta.dir, "..", "server", "Dockerfile")

/**
 * Логические инструкции Dockerfile: line-continuations (`\` + перенос)
 * склеены в одну строку, комментарии выброшены — чтобы парсер не видел
 * упоминания маунтов в комментариях.
 */
function instructions(src: string): string[] {
  const joined = src
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("#"))
    .join("\n")
    .replace(/\\\s*\n\s*/g, " ")
  return joined
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
}

/** RUN-инструкции с хотя бы одним `--mount=type=cache`. */
function cacheMountRuns(insts: string[]): string[] {
  return insts.filter(
    (inst) => inst.startsWith("RUN ") && inst.includes("--mount=type=cache"),
  )
}

/** Флаги `--mount=...` внутри инструкции (значение до пробела/конца строки). */
function mountFlags(inst: string): string[] {
  return [...inst.matchAll(/--mount=(\S+)/g)].map((m) => m[1])
}

function flagParam(flag: string, key: string): string | null {
  const m = flag.match(new RegExp(`(?:^|,)${key}=([^,]*)`))
  return m ? m[1] : null
}

const insts = instructions(readFileSync(DOCKERFILE_PATH, "utf8"))
const runs = cacheMountRuns(insts)
const mounts = runs.flatMap((inst) => mountFlags(inst).map((flag) => ({ inst, flag })))
const cacheMounts = mounts.filter(({ flag }) => flag.startsWith("type=cache"))

describe("server/Dockerfile — BuildKit cache mounts (uwuwu#1162)", () => {
  test("cargo registry и build target кэшируются: оба маунта на месте", () => {
    // Пин самой фичи: снос маунтов = возврат ~20-минутного ребилда lance/
    // arrow/tokio на каждую правку — регресс, тест обязан это поймать.
    const targets = cacheMounts.map(({ flag }) => flagParam(flag, "target"))
    expect(targets).toContain("/usr/local/cargo/registry")
    expect(targets).toContain("/build/target")
  })

  test("каждый cache-маунт с явным id — безымянный = невалидный Dockerfile на Railway (root cause 275d5fd5)", () => {
    // Railway-валидатор: "flag '--mount=type=cache,...' is missing an id
    // argument" — билд падает на парсинге, не «молча не кэширует».
    const unnamed = cacheMounts.filter(({ flag }) => !flagParam(flag, "id"))
    expect(unnamed).toEqual([])
  })

  test("id непустой и target задан у каждого маунта", () => {
    // target без значения = маунт не туда, куда cargo пишет registry/target:
    // кеш молча не работает. id="" = то же, что безымянный.
    for (const { flag } of cacheMounts) {
      expect(flagParam(flag, "id")).not.toBe("")
      expect(flagParam(flag, "target")).not.toBe(null)
      expect(flagParam(flag, "target")).not.toBe("")
    }
  })

  test("cp-трюк: бинарь вынесен из target-маунта в обычный слой до конца RUN", () => {
    // Содержимое cache mount не попадает в слои образа: без cp бинарь
    // физически отсутствует в stage build — COPY --from нечего копировать.
    for (const inst of runs) {
      expect(inst).toContain("cp /build/target/release/uwuwu-server /build/uwuwu-server")
    }
  })

  test("stage runtime копирует бинарь из слоя (/build/uwuwu-server), не из маунта", () => {
    // Пин парной половины cp-трюка: COPY --from=build /build/target/...
    // при маунтованном target — регресс к гоче задачи.
    const copies = insts.filter((inst) => inst.startsWith("COPY --from=build"))
    expect(copies.length).toBe(1)
    expect(copies[0]).toContain("/build/uwuwu-server")
    expect(copies[0]).not.toContain("/build/target/release/")
  })
})
