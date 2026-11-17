// relay-deploy-contract.test.ts — пин контракта деплой-пути релея после
// инцидента Railway #1201 (задача uwuwu#1202). Два корня падения:
//  1) дубликат server/ops/relay/Dockerfile — distroless-база rapiz1/rathole
//     без /bin/sh: `CMD ["/bin/sh", …]` скармливал скрипт самому rathole
//     («Found argument '/entrypoint.sh'»), ENTRYPOINT не сброшен, healthcheck
//     нет — Railway красил краш-деплой в SUCCESS. Снесён, канон — корневой
//     Dockerfile (multi-stage + debian:12-slim + сброс ENTRYPOINT +
//     healthcheck control-порта);
//  2) деплой из Windows-чекаута: text=auto держит LF только в индексе,
//     ворктри с autocrlf отдавал entrypoint.sh как CRLF — dash падал на
//     «set: Illegal option -». Лечится строкой `*.sh text eol=lf` в
//     .gitattributes (расширение ADR-060).
// Статический тест: файлы читаются локально, сети нет (конвенция
// dockerfile-cache-mounts.test.ts); поведение сборки доказывают
// podman-прогон и прод-деплой Railway (приёмка владельцем, пост-мердж).
import { describe, expect, test } from "bun:test"
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs"
import { join } from "node:path"

// Путь от lib/ вверх: тест должен работать при любом cwd (bun test lib/
// из корня репо, но и точечный запуск bun test lib/x.test.ts — тоже)
const ROOT = join(import.meta.dir, "..")
const RELAY_DIR = join(ROOT, "server", "ops", "relay")

/** Все *.sh в дереве (ворктри), кроме служебных каталогов — семейство
 * скриптов, покрытое атрибутом `*.sh text eol=lf`. */
function shellScripts(dir: string, base = dir): string[] {
  const out: string[] = []
  for (const name of readdirSync(dir)) {
    const full = join(dir, name)
    if (name === ".git" || name === "node_modules") continue
    if (statSync(full).isDirectory()) out.push(...shellScripts(full, base))
    else if (name.endsWith(".sh")) out.push(full)
  }
  return out
}

/** Число CR-байт (0x0d) в файле — маркер CRLF-ворктри. */
function crBytes(path: string): number {
  const buf = readFileSync(path)
  let n = 0
  for (const byte of buf) if (byte === 0x0d) n++
  return n
}

const attrs = readFileSync(join(ROOT, ".gitattributes"), "utf8")
const rootDockerfile = readFileSync(join(ROOT, "Dockerfile"), "utf8")

describe("релей — контракт деплой-пути (uwuwu#1202)", () => {
  test("дубликат server/ops/relay/Dockerfile снесён и не resurrectится", () => {
    // Пин корня №1: файл-ловушка деплой-пути (distroless без /bin/sh, без
    // healthcheck). Возврат файла = возврат краш-деплоя «SUCCESS».
    expect(existsSync(join(RELAY_DIR, "Dockerfile"))).toBe(false)
  })

  test("в server/ops/relay нет ни одного Dockerfile — канон деплой-пути единственный (корень репо)", () => {
    // Класс-уровень пина: любой новый Dockerfile в relay-каталоге — тот же
    // механизм ловушки (альтернативный деплой-путь мимо канона).
    const dockers = readdirSync(RELAY_DIR).filter((name) => name.startsWith("Dockerfile"))
    expect(dockers).toEqual([])
  })

  test("корневой Dockerfile держит контракт канона: ENTRYPOINT сброшен, healthcheck control-порта на месте", () => {
    // Без сброса ENTRYPOINT CMD уходит в rathole distroless-базы; без
    // HEALTHCHECK Railway красит мгновенный краш в SUCCESS (инцидент 15.11).
    expect(rootDockerfile).toMatch(/^ENTRYPOINT \[\]/m)
    expect(rootDockerfile).toContain("HEALTHCHECK")
    expect(rootDockerfile).toContain("dev/tcp/127.0.0.1/2333")
  })

  test("носитель генерации конфига на месте: entrypoint.sh exec-ит rathole (его копирует корневой Dockerfile)", () => {
    // Снос дубля не должен был задеть носитель: COPY server/ops/relay/entrypoint.sh.
    const sh = readFileSync(join(RELAY_DIR, "entrypoint.sh"), "utf8")
    expect(sh).toContain("exec rathole")
  })

  test("`.gitattributes` держит `*.sh text eol=lf` — CRLF-ворктри Windows-чекаутов не возвращается", () => {
    // Пин корня №2: text=auto страхует только индекс; без eol=lf ручной
    // railway up из Windows-чекаута кладёт CRLF-скрипт в билд-контекст.
    expect(attrs).toMatch(/^\*\.sh text eol=lf$/m)
  })

  test("все *.sh в ворктри — чистый LF: ни одного CR-байта (деплой-критично для entrypoint.sh)", () => {
    // Пин на классе (семейство всех скриптов репы), не только головной кейс:
    // CRLF в dash ловится в рантайме («set: Illegal option -»), не в парсинге.
    const crlf = shellScripts(ROOT).filter((path) => crBytes(path) > 0)
    expect(crlf).toEqual([])
  })
})
