// bots-lib.test.ts — тесты чистых функций реестра ботов (bun test, без сети):
// парсинг док-спеки, диф жизненного цикла, seed-контракт directory.

import { describe, test, expect } from "bun:test"
import {
  decideActions, defTargetDirectory, liveSessionDirectory, parseBotDoc, parseModel, runtimeHashOf,
  type BotObservation, type RegistryCacheEntry,
} from "./bots-lib"

const specMd = (over: { title?: string; bot?: string; def?: string } = {}): string => {
  const botBlock = over.bot ?? [
    "bot:",
    '  token: "123:AAA"',
    "  owner: yurvon_screamo",
    "  model: zai-coding-plan/glm-5.3-flash#max",
    "  workspace: /home/yurvon/origa/marketing",
    "  enabled: true",
  ].join("\n")
  // форма export-обвязки ядра: frontmatter + title-H1 + тело (Спека + методичка)
  return [
    "---",
    `title: ${over.title ?? "Бот: origa-marketing"}`,
    "date: 2026-11-13T10:00:00+09:00",
    "lastmod: 2026-11-13T10:00:00+09:00",
    "---",
    "",
    `# ${over.title ?? "Бот: origa-marketing"}`,
    "",
    "## Спека",
    "",
    "```yaml",
    botBlock,
    "```",
    "",
    over.def ?? "# Маркетолог Origa\n\nТы в личке владельца.",
  ].join("\n")
}

const obs = (slug: string, updatedAt: string, md: string): BotObservation => {
  const parsed = parseBotDoc(`doc-${slug}`, md)
  const model = parsed.model !== undefined ? parseModel(parsed.model) : undefined
  if (model === undefined || model === null) throw new Error(`фикстура без модели: ${parsed.model ?? ""}`)
  return {
    updatedAt,
    spec: {
      slug,
      docId: `doc-${slug}`,
      token: parsed.token,
      owner: parsed.owner,
      model,
      ...(parsed.workspace !== undefined ? { workspace: parsed.workspace } : {}),
      enabled: parsed.enabled,
      def: parsed.def,
    },
  }
}

const cacheEntry = (o: BotObservation): RegistryCacheEntry => ({
  docId: o.spec.docId,
  updatedAt: o.updatedAt,
  runtimeHash: runtimeHashOf(o.spec.token, o.spec.owner, o.spec.model, o.spec.workspace, o.spec.enabled),
})

// ——— parseBotDoc ———

describe("parseBotDoc", () => {
  test("полная спека: слаг, токен, owner, модель с variant, workspace, тело-методичка", () => {
    const p = parseBotDoc("d1", specMd())
    expect(p.slug).toBe("origa-marketing")
    expect(p.token).toBe("123:AAA")
    expect(p.owner).toBe("yurvon_screamo")
    expect(p.model).toBe("zai-coding-plan/glm-5.3-flash#max")
    expect(p.workspace).toBe("/home/yurvon/origa/marketing")
    expect(p.enabled).toBe(true)
    expect(p.def).toContain("Маркетолог Origa")
  })

  test("кавычки опциональны, enabled: false парсится", () => {
    const p = parseBotDoc("d1", specMd({ bot: "bot:\n  token: 123:AAA\n  owner: owner_x\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: false" }))
    expect(p.token).toBe("123:AAA")
    expect(p.owner).toBe("owner_x")
    expect(p.model).toBe("zai-coding-plan/glm-5.3-flash")
    expect(p.enabled).toBe(false)
    expect(p.workspace).toBeUndefined()
  })

  test("модель без слэша, пустой workspace — throw; отсутствующая модель/токен/owner/тело — throw", () => {
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token: t\n  owner: o\n  model: zai-coding-plan/glm-5.3-flash\n  workspace: \"\"\n  enabled: true" }))).toThrow("workspace")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  owner: x\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: true" }))).toThrow("token")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token: t\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: true" }))).toThrow("owner")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token: t\n  owner: o\n  enabled: true" }))).toThrow("bot.model")
    expect(() => parseBotDoc("d1", specMd({ def: "" }))).toThrow("методичка")
  })

  test("не-спека (тайтл без «Бот: »), нет секции Спека, нет фенса, незакрытый фенс — throw", () => {
    expect(() => parseBotDoc("d1", specMd({ title: "Что-то другое" }))).toThrow("Бот: ")
    expect(() => parseBotDoc("d1", "---\ntitle: Бот: x\n---\n\n# x\n\nТело без спеки")).toThrow("Спека")
    expect(() => parseBotDoc("d1", "---\ntitle: Бот: x\n---\n\n# x\n\n## Спека\n\nнет фенса")).toThrow("yaml")
    expect(() => parseBotDoc("d1", "---\ntitle: Бот: x\n---\n\n# x\n\n## Спека\n\n```yaml\nbot:\n  token: t\n")).toThrow("закрыт")
  })

  test("вступление до Спеки входит в методичку; битые строки блока, дубликат и неизвестный ключ — throw", () => {
    const p = parseBotDoc("d1", specMd({ def: "Вступление.\n\n## Методичка\n\nТекст." }))
    expect(p.def).toContain("Вступление.")
    expect(p.def).toContain("Методичка")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token: t\n  token: t2\n  owner: o\n  enabled: true" }))).toThrow("дубликат")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token: t\n  owner: o\n  enabled: true\n  secret: 1" }))).toThrow("неизвестный ключ")
    expect(() => parseBotDoc("d1", specMd({ bot: "bot:\n  token === мусор\n  owner: o\n  enabled: true" }))).toThrow("битая строка")
  })

  test("пустое тело-методичка — throw (fail-loud супервизора)", () => {
    expect(() => parseBotDoc("d1", specMd({ def: "" }))).toThrow("методичка")
  })

  test("битый frontmatter — throw", () => {
    expect(() => parseBotDoc("d1", "не frontmatter")).toThrow("frontmatter")
  })
})

describe("parseModel", () => {
  test("provider/id и provider/id#variant", () => {
    expect(parseModel("zai-coding-plan/glm-5.3-flash")).toEqual({ providerID: "zai-coding-plan", id: "glm-5.3-flash" })
    expect(parseModel("zai-coding-plan/glm-5.3-flash#max")).toEqual({ providerID: "zai-coding-plan", id: "glm-5.3-flash", variant: "max" })
    expect(parseModel("без-слэша")).toBeNull()
  })
})

// ——— decideActions ———

describe("decideActions", () => {
  const S = specMd()
  const live = (slugs: string[]): Set<string> => new Set(slugs)

  test("новый enabled-док — spawn; новый disabled — без действий; мёртвый ребёнок при живой спеке — spawn", () => {
    const a = decideActions({}, [obs("x", "20261113100000", S)], live([]))
    expect(a).toEqual([{ kind: "spawn", slug: "x" }])
    const b = decideActions({}, [obs("x", "20261113100000", specMd({ bot: "bot:\n  token: t\n  owner: o\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: false" }))], live([]))
    expect(b).toEqual([])
    const prev = obs("x", "20261113100000", S)
    const c = decideActions({ x: cacheEntry(prev) }, [obs("x", "20261113100000", S)], live([]))
    expect(c).toEqual([{ kind: "spawn", slug: "x" }])
  })

  test("снесённый док при живом ребёнке — stop; при мёртвом — без действий", () => {
    const prev = obs("x", "20261113100000", S)
    const cache = { x: cacheEntry(prev) }
    expect(decideActions(cache, [], live(["x"]))).toEqual([{ kind: "stop", slug: "x" }])
    expect(decideActions(cache, [], live([]))).toEqual([])
  })

  test("enabled: false у живого ребёнка — stop", () => {
    const prev = obs("x", "20261113100000", S)
    const off = obs("x", "20261113100100", specMd({ bot: "bot:\n  token: t\n  owner: o\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: false" }))
    expect(decideActions({ x: cacheEntry(prev) }, [off], live(["x"]))).toEqual([{ kind: "stop", slug: "x" }])
  })

  test("смена рантайм-существенного (токен) — restart; смена тела — def-update; без изменений — тихо", () => {
    const prev = obs("x", "20261113100000", S)
    const cache = { x: cacheEntry(prev) }
    const tokenChanged = obs("x", "20261113100100", specMd({ bot: "bot:\n  token: 999:BBB\n  owner: yurvon_screamo\n  model: zai-coding-plan/glm-5.3-flash#max\n  workspace: /home/yurvon/origa/marketing\n  enabled: true" }))
    expect(decideActions(cache, [tokenChanged], live(["x"])).at(0)?.kind).toBe("restart")
    const defChanged = obs("x", "20261113100200", specMd({ def: "# Новая методичка" }))
    expect(decideActions(cache, [defChanged], live(["x"]))).toEqual([{ kind: "def-update", slug: "x" }])
    const same = obs("x", "20261113100000", S)
    expect(decideActions(cache, [same], live(["x"]))).toEqual([])
  })

  test("правка тела в disabled-доке — без действий (гасим, не рестартим)", () => {
    const prev = obs("x", "20261113100000", S)
    const off = obs("x", "20261113100300", specMd({ bot: "bot:\n  token: t\n  owner: o\n  model: zai-coding-plan/glm-5.3-flash\n  enabled: false", def: "# Другая методичка" }))
    expect(decideActions({ x: cacheEntry(prev) }, [off], live(["x"]))).toEqual([{ kind: "stop", slug: "x" }])
  })
})

// ——— seed-контракт directory ———

describe("seed-контракт", () => {
  test("directory живой сессии читается из GET /session (data.location.directory)", () => {
    expect(liveSessionDirectory({ data: { id: "ses_1", location: { directory: "/home/yurvon/origa/marketing" } } })).toBe("/home/yurvon/origa/marketing")
    expect(liveSessionDirectory({ id: "ses_1", location: { directory: "/x" } })).toBe("/x")
    expect(liveSessionDirectory({ data: { id: "ses_1" } })).toBeNull()
    expect(liveSessionDirectory(null)).toBeNull()
    expect(liveSessionDirectory("мусор")).toBeNull()
  })

  test("directory сессии wins при расхождении с workspace спеки — warning", () => {
    const r = defTargetDirectory("/home/yurvon/origa/marketing", "/old/directory")
    expect(r.directory).toBe("/old/directory")
    expect(r.warning).toContain("/old/directory")
    expect(r.warning).toContain("/home/yurvon/origa/marketing")
  })

  test("совпадение или null-сессия — workspace спеки без warning", () => {
    expect(defTargetDirectory("/w", "/w").warning).toBeUndefined()
    expect(defTargetDirectory("/w", null).directory).toBe("/w")
    expect(defTargetDirectory(undefined, null).directory).toBe("")
  })
})
