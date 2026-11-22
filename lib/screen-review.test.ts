// screen-review.test.ts — юнит-тесты ядра screen_review (bun test, без сети:
// API подменяется postJson, ФС — readImageBase64). Контракт ответа Decisions
// API — константа ANSWER_CONTRACT_FIELDS; нехватка поля = громкая ошибка, не
// пустой отчёт. Живой контракт Gemini зафиксирован пробами 20.11.2026 (#1362,
// срез 1): фикстура — живой ответ gemini-decision-1 на синтетический скрин
// с заведомыми дефектами (двойная рамка, карточка-в-карточке, сдвиг,
// налезание, слабый контраст — все посажены намеренно, ground truth известен).
import { describe, expect, test } from "bun:test"
import {
  ANSWER_CONTRACT_FIELDS,
  buildRequest,
  CHECKLISTS,
  DEFAULT_THRESHOLD,
  parseAnswers,
  problemProbability,
  resolveMimeType,
  resolveQuestions,
} from "./screen-review-core"
import { DECISIONS_URL, DEFAULT_MODEL, runScreenReview, type ScreenReviewDeps } from "./screen-review"
import { renderReport, renderVerdictSection, scoreVerdictLabel } from "./screen-review-render"

// Живой ответ gemini-decision-1 (проба 20.11.2026, POST /v1beta/decisions,
// 1634 input-токена, ~1.0 с — все 7 вопросов ui-slop, три типа ответа).
const GEMINI_OK = JSON.stringify({
  answers: {
    double_frame: { confidence: 0.7918995921976731, noul: 0.7918995921976731, type: "noul" },
    card_in_card: { confidence: 0.509538286627421, noul: 0.509538286627421, type: "noul" },
    alignment: { confidence: 0.5241035189477238, noul: 0.5241035189477238, type: "noul" },
    overlap: { confidence: 0.7465647392217101, noul: 0.7465647392217101, type: "noul" },
    contrast: { confidence: 0.5092533537767471, noul: 0.5092533537767471, type: "noul" },
    main_defect: {
      confidence: 0.5548847972728546,
      choice: "overlap",
      probabilities: {
        overlap: 0.5548847972728546, contrast: 0.1295350356982163, double_frame: 0.11296824431662063,
        other: 0.10998025809101027, alignment: 0.02326296686378967, none: 0.01693529270245775,
        card_in_card: 0.05243340505505083,
      },
      type: "choice",
    },
    severity: {
      confidence: 0.5503023347831089,
      score: 2.3595156604574994,
      probabilities: { "0": 0.036449140141597364, "1": 0.11788839404241439, "2": 0.29536013103287934, "3": 0.5503023347831089 },
      legend: { "0": "clean", "1": "minor flaws", "2": "obvious defects", "3": "critical" },
      type: "score",
    },
  },
  usage: { total_tokens: 1634, total_input_tokens: 1634, total_output_tokens: 0 },
})

describe("parseAnswers — контракт ответа Decisions API (анти-дрейф)", () => {
  test("живой ответ Gemini парсится: три типа, total_input_tokens", () => {
    const { answers, inputTokens } = parseAnswers(GEMINI_OK, "test")
    expect(inputTokens).toBe(1634)
    expect(answers.double_frame).toEqual({ type: "noul", noul: 0.7918995921976731 })
    expect(answers.main_defect?.type).toBe("choice")
    expect(answers.severity?.type).toBe("score")
  })

  test.each([
    ["the response body is not JSON", "{oops", "the response body is not JSON"],
    ["answers отсутствует", JSON.stringify({ usage: { total_input_tokens: 1 } }), "no answers map"],
    ["noul без числа", JSON.stringify({ answers: { q: { type: "noul" } } }), "type=noul without a noul number"],
    [
      "choice без probabilities",
      JSON.stringify({ answers: { q: { type: "choice", choice: "a" } } }),
      "type=choice without choice/probabilities",
    ],
    ["score без числа", JSON.stringify({ answers: { q: { type: "score" } } }), "type=score without a score number"],
    [
      "неизвестный type",
      JSON.stringify({ answers: { q: { type: "box", box: [] } } }),
      "unknown type=box",
    ],
  ])("неконтрактный ответ «%s» → громкая ошибка с перечнем полей", (_name, raw, what) => {
    const err = (() => {
      try {
        parseAnswers(raw, "test")
        return null
      } catch (e) {
        return e as Error
      }
    })()
    expect(err).not.toBeNull()
    expect(err?.message).toContain(what)
    for (const field of ANSWER_CONTRACT_FIELDS) expect(err?.message).toContain(field.split("[")[0])
  })

  test("неполный ответ (подмножество вопросов) → контрактная ошибка с id недостающих", () => {
    const partial = JSON.stringify({
      answers: { double_frame: { type: "noul", noul: 0.5 } },
      usage: { total_input_tokens: 1 },
    })
    const err = (() => {
      try {
        parseAnswers(partial, "test", ["double_frame", "main_defect", "severity"])
        return null
      } catch (e) {
        return e as Error
      }
    })()
    expect(err?.message).toContain("the model did not answer all questions")
    expect(err?.message).toContain("main_defect, severity")
  })

  test("полный ответ при сверке с вопросами проходит", () => {
    const { answers } = parseAnswers(GEMINI_OK, "test", Object.keys(resolveQuestions({})))
    expect(Object.keys(answers)).toHaveLength(7)
  })
})

describe("resolveQuestions — чек-листы, inline-вопросы и гарды ограничений API", () => {
  test("дефолт — встроенный ui-slop, 7 вопросов из постановки", () => {
    const q = resolveQuestions({})
    expect(Object.keys(q).sort()).toEqual(
      ["alignment", "card_in_card", "contrast", "double_frame", "main_defect", "overlap", "severity"].sort(),
    )
  })

  test("неизвестное имя чек-листа → громкая ошибка со списком встроенных", () => {
    expect(() => resolveQuestions({ checklist: "perf" })).toThrow("unknown checklist «perf»")
  })

  test("inline questions заменяют чек-лист", () => {
    const q = resolveQuestions({ questions: { only: { type: "noul", instructions: "тест" } } })
    expect(Object.keys(q)).toEqual(["only"])
  })

  test("оба источника сразу → громкая ошибка, не молчаливый приоритет", () => {
    expect(() => resolveQuestions({ checklist: "ui-slop", questions: { x: { type: "noul", instructions: "x" } } })).toThrow(
      "keep one source of questions",
    )
  })

  test("пустой inline-чек-лист → громкая ошибка лимита 1–64", () => {
    expect(() => resolveQuestions({ questions: {} })).toThrow("1–64")
  })

  test("более 64 вопросов → громкая ошибка", () => {
    const many = Object.fromEntries(
      Array.from({ length: 65 }, (_, i) => [`q${i}`, { type: "noul" as const, instructions: "x" }]),
    )
    expect(() => resolveQuestions({ questions: many })).toThrow("1–64")
  })

  // ——— гарды ограничений Decisions API (пробы 20.11, #1362) ———

  test.each([
    ["кириллица в id", { "вопрос_1": { type: "noul" as const, instructions: "x" } }],
    ["пробел в id", { "my question": { type: "noul" as const, instructions: "x" } }],
    ["id длиннее 64", { [`${"q".repeat(65)}`]: { type: "noul" as const, instructions: "x" } }],
  ])("гард id: %s → громкая ошибка входа до сети", (_name, questions) => {
    expect(() => resolveQuestions({ questions })).toThrow("[A-Za-z0-9_-]{1,64}")
  })

  test("choice без criteria → громкая ошибка (API отвечает 400 без них)", () => {
    expect(() => resolveQuestions({ questions: { q: { type: "choice", instructions: "x" } } })).toThrow(
      "criteria is required for type=choice",
    )
  })

  test("score без criteria → громкая ошибка", () => {
    expect(() => resolveQuestions({ questions: { q: { type: "score", instructions: "x" } } })).toThrow(
      "criteria is required for type=score",
    )
  })

  test("choice с одной опцией → громкая ошибка минимума 2–255", () => {
    expect(() =>
      resolveQuestions({ questions: { q: { type: "choice", instructions: "x", criteria: { a: "описание" } } } }),
    ).toThrow("2–255 options, got 1")
  })

  test("choice с ключом длиннее 128 символов → громкая ошибка (400 живьём, проба 20.11)", () => {
    const longKey = "k".repeat(129)
    expect(() =>
      resolveQuestions({ questions: { q: { type: "choice", instructions: "x", criteria: { a: "x", [longKey]: "y" } } } }),
    ).toThrow("is outside the 1–128 character limit")
  })

  test("choice с пустым ключом опции → громкая ошибка без вранья про длину", () => {
    expect(() =>
      resolveQuestions({ questions: { q: { type: "choice", instructions: "x", criteria: { "": "описание", a: "x" } } } }),
    ).toThrow("(empty)")
  })

  test("score с одним уровнем → громкая ошибка минимума 2–255", () => {
    expect(() =>
      resolveQuestions({ questions: { q: { type: "score", instructions: "x", criteria: ["один"] } } }),
    ).toThrow("2–255 levels, got 1")
  })

  test("noul с массивом criteria → громкая ошибка shape", () => {
    expect(() =>
      resolveQuestions({ questions: { q: { type: "noul", instructions: "x", criteria: ["нет", "да"] } } }),
    ).toThrow("{true_description, false_description}")
  })

  test("noul с чужими ключами criteria → громкая ошибка shape", () => {
    expect(() =>
      resolveQuestions({ questions: { q: { type: "noul", instructions: "x", criteria: { yes: "да", no: "нет" } } } }),
    ).toThrow("the keys true_description and false_description")
  })

  test("noul с правильным shape criteria проходит (принят API живьём, проба 20.11)", () => {
    const q = resolveQuestions({
      questions: {
        q: { type: "noul", instructions: "x", criteria: { true_description: "да", false_description: "нет" } },
      },
    })
    expect(q.q.criteria).toEqual({ true_description: "да", false_description: "нет" })
  })

  test("ключи опций choice со свободным charset проходят (пробел/кириллица приняты API, проба 20.11)", () => {
    const q = resolveQuestions({
      questions: { q: { type: "choice", instructions: "x", criteria: { "my option": "обычная", "вариант_2": "нажатая" } } },
    })
    expect(Object.keys(q.q.criteria as Record<string, string>)).toEqual(["my option", "вариант_2"])
  })
})

describe("buildRequest — input-блоки text+image, questions map'ом", () => {
  test("сборка запроса Decisions API", () => {
    const q = resolveQuestions({})
    const req = buildRequest("gemini-decision-1", "state", "B64", "image/png", q)
    expect(req.model).toBe("gemini-decision-1")
    expect(req.input).toEqual([
      { type: "text", text: "state" },
      { type: "image", data: "B64", mime_type: "image/png" },
    ])
    expect(Array.isArray(req.questions)).toBe(false)
    expect(req.questions.double_frame?.type).toBe("noul")
  })

  test("ui-slop: choice-критерии содержат none, score-уровни от меньшего к большему", () => {
    const slop = CHECKLISTS["ui-slop"]
    expect(slop.main_defect.criteria).toHaveProperty("none")
    expect(slop.severity.criteria).toEqual(["clean", "minor flaws", "obvious defects", "critical"])
  })
})

describe("resolveMimeType — расширение в mime (image-блок API требует mime_type)", () => {
  test.each([
    ["/shots/a.png", "image/png"],
    ["/shots/a.JPG", "image/jpeg"],
    ["/shots/a.jpeg", "image/jpeg"],
    ["/shots/a.webp", "image/webp"],
  ])("%s → %s", (path, mime) => {
    expect(resolveMimeType(path)).toBe(mime)
  })

  test("неизвестное расширение → громкая ошибка со списком поддерживаемых", () => {
    expect(() => resolveMimeType("/shots/a.gif")).toThrow(".png, .jpg/.jpeg, .webp")
  })
})

describe("problemProbability — единая семантика «вероятность проблемы»", () => {
  test("noul — сама вероятность", () => {
    expect(problemProbability({ type: "noul", noul: 0.85 })).toBe(0.85)
  })

  test("choice — max P по не-none критериям", () => {
    const p = problemProbability({
      type: "choice",
      choice: "double_frame",
      probabilities: { none: 0.37, double_frame: 0.55, other: 0.07 },
    })
    expect(p).toBeCloseTo(0.55)
  })

  test("choice без none — max по всем", () => {
    const p = problemProbability({ type: "choice", choice: "b", probabilities: { a: 0.2, b: 0.7 } })
    expect(p).toBeCloseTo(0.7)
  })

  test("score — 1 − P(чисто)", () => {
    const p = problemProbability({
      type: "score",
      score: 1.27,
      probabilities: { "0": 0.3, "1": 0.13, "2": 0.57 },
    })
    expect(p).toBeCloseTo(0.7)
  })

  test("score без probabilities → null (посчитать не из чего)", () => {
    expect(problemProbability({ type: "score", score: 1 })).toBeNull()
  })
})

describe("scoreVerdictLabel — подпись уровня по legend", () => {
  const legend = { "0": "clean", "1": "minor flaws", "2": "obvious defects", "3": "critical" }

  test("целый score — точный уровень", () => {
    expect(scoreVerdictLabel({ type: "score", score: 2, legend })).toBe("obvious defects")
  })

  test("дробный score — диапазон «X → Y»", () => {
    expect(scoreVerdictLabel({ type: "score", score: 1.27, legend })).toBe("minor flaws → obvious defects")
  })

  test("без legend → null", () => {
    expect(scoreVerdictLabel({ type: "score", score: 1 })).toBeNull()
  })
})

describe("renderVerdictSection — таблица и флаги порога (живой вердикт Gemini)", () => {
  const { answers } = parseAnswers(GEMINI_OK, "test")
  const verdict = { imagePath: "/shots/dashboard.png", imageName: "dashboard.png", answers, inputTokens: 1634 }

  test("флаги P>0.6: double_frame 0.79, overlap 0.75, severity 0.96; тонкие ~0.5 — без флага", () => {
    const md = renderVerdictSection(verdict, "state", DEFAULT_THRESHOLD)
    expect(md).toContain("### Screenshot: dashboard.png")
    expect(md).toContain("/shots/dashboard.png")
    expect(md).toContain("input_tokens: 1634")
    expect(md).toContain("| double_frame | noul | 0.79 | 0.79 | ⚠️ |")
    expect(md).toContain("| overlap | noul | 0.75 | 0.75 | ⚠️ |")
    expect(md).toContain("| main_defect | choice | overlap (0.55) · conf 0.55 | 0.55 | — |")
    expect(md).toContain("| severity | score | 2.36 — obvious defects → critical | 0.96 | ⚠️ |")
    expect(md).toContain("**Signals** (P > 0.6)")
    expect(md).toContain("- severity — P 0.96")
    expect(md).toContain("- double_frame — P 0.79")
  })

  test("нет сигналов — явная строка, не тишина", () => {
    const clean = { ...verdict, answers: { q: { type: "noul" as const, noul: 0.1 } } }
    const md = renderVerdictSection(clean, "state", DEFAULT_THRESHOLD)
    expect(md).toContain("**No signals** — no question exceeded the 0.6 threshold.")
  })
})

describe("renderReport — шапка и напоминание роли скринера", () => {
  test("шапка с моделью и чек-листом, хвост с ролью скринера", () => {
    const md = renderReport(["section"], { model: DEFAULT_MODEL, checklistName: "ui-slop" }, 0.6)
    expect(md).toContain("## screen_review (gemini-decision-1, checklist ui-slop, threshold 0.6)")
    expect(md).toContain("screener/aimer, not the final judge")
    expect(md).toContain("section")
  })
})

// ——— оркестрация с подменой сети/ФС ———

const okDeps = (screens: string[]): ScreenReviewDeps => ({
  readImageBase64: async (path) => {
    if (!screens.includes(path)) throw new Error(`enoent: ${path}`)
    return "B64"
  },
  postJson: async () => ({ status: 200, body: GEMINI_OK }),
  config: { apiKey: "test-key", model: "gemini-decision-1" },
})

describe("runScreenReview — пер-скрин семантика, auth и валидация входа", () => {
  test("два скрина → две секции, атрибуция вердиктов к скрину", async () => {
    const md = await runScreenReview({ images: ["/a.png", "/b.png"] }, okDeps(["/a.png", "/b.png"]))
    expect(md).toContain("### Screenshot: a.png")
    expect(md).toContain("### Screenshot: b.png")
    expect(md).toContain("Flagged screenshots")
    expect(md).toContain("`a.png`, `b.png`")
  })

  test("пустой images → громкая ошибка", async () => {
    await expect(runScreenReview({ images: [] }, okDeps([]))).rejects.toThrow("a non-empty array of screenshot paths")
  })

  test("несуществующий скрин → громкая ошибка с путём, не тихий пропуск", async () => {
    await expect(runScreenReview({ images: ["/missing.png"] }, okDeps([]))).rejects.toThrow("/missing.png")
  })

  test("нет GEMINI_API_KEY → громкая ошибка конфигурации до сети", async () => {
    const deps = okDeps(["/a.png"])
    deps.config = { apiKey: "", model: "gemini-decision-1" }
    await expect(runScreenReview({ images: ["/a.png"] }, deps)).rejects.toThrow("GEMINI_API_KEY is not set")
  })

  test("ключ уходит в заголовок x-goog-api-key (не в URL и не в тело)", async () => {
    const seen: Array<{ url: string; headers: Record<string, string> }> = []
    const deps = okDeps(["/a.png"])
    deps.postJson = async (url, _body, headers) => {
      seen.push({ url, headers })
      return { status: 200, body: GEMINI_OK }
    }
    await runScreenReview({ images: ["/a.png"] }, deps)
    expect(seen[0]?.url).toBe(DECISIONS_URL)
    expect(seen[0]?.headers["x-goog-api-key"]).toBe("test-key")
  })

  test("decisions api 500 → честный текст ошибки с телом ответа", async () => {
    const deps = okDeps(["/a.png"])
    deps.postJson = async () => ({ status: 500, body: '{"error":{"message":"internal error"}}' })
    await expect(runScreenReview({ images: ["/a.png"] }, deps)).rejects.toThrow("decisions api 500")
    await expect(runScreenReview({ images: ["/a.png"] }, deps)).rejects.toThrow("internal error")
  })

  test("inline-вопросы уходят в запрос, отчёт помечен <inline>", async () => {
    const seenRequests: unknown[] = []
    const deps = okDeps(["/a.png"])
    deps.postJson = async (_url, body) => {
      seenRequests.push(body)
      const ids = Object.keys(body.questions)
      return {
        status: 200,
        body: JSON.stringify({
          answers: Object.fromEntries(ids.map((id) => [id, { type: "noul", noul: 0.1 }])),
          usage: { total_input_tokens: 100 },
        }),
      }
    }
    const md = await runScreenReview(
      { images: ["/a.png"], questions: { only: { type: "noul", instructions: "тест" } } },
      deps,
    )
    expect(md).toContain("checklist <inline>")
    const req = seenRequests[0] as { questions: Record<string, unknown>; input: unknown[] }
    expect(Object.keys(req.questions)).toEqual(["only"])
    expect(md).toContain("| only | noul |")
    expect(req.input).toEqual([
      { type: "text", text: "Screenshot of a user interface" },
      { type: "image", data: "B64", mime_type: "image/png" },
    ])
  })

  test("кастомный порог передаётся в рендер", async () => {
    const md = await runScreenReview({ images: ["/a.png"], threshold: 0.9 }, okDeps(["/a.png"]))
    expect(md).toContain("threshold 0.9")
    // живой вердикт: максимум «вероятности проблемы» — severity 0.96; порог 0.97 выше всех — флагов нет
    const md2 = await runScreenReview({ images: ["/a.png"], threshold: 0.97 }, okDeps(["/a.png"]))
    expect(md2).toContain("No flagged screenshots.")
  })

  test.each([
    ["отрицательный", -0.1],
    ["больше 1", 1.5],
    ["не число", Number.NaN],
  ])("threshold %s → громкая ошибка диапазона", async (_name, threshold) => {
    await expect(runScreenReview({ images: ["/a.png"], threshold }, okDeps(["/a.png"]))).rejects.toThrow(
      "threshold — a number 0..1",
    )
  })

  test("относительный путь → громкая ошибка с путём (готч cwd тула — сервер)", async () => {
    await expect(runScreenReview({ images: ["shots/a.png"] }, okDeps([]))).rejects.toThrow(
      "screenshot paths must be absolute",
    )
  })

  test("неизвестное расширение → громкая ошибка до чтения файла", async () => {
    await expect(runScreenReview({ images: ["/a.gif"] }, okDeps([]))).rejects.toThrow("unknown screenshot extension")
  })

  test("сетевое исключение → обёртка со скрином, URL и причиной", async () => {
    const deps = okDeps(["/a.png"])
    const cause = new Error("connect ECONNREFUSED 127.0.0.1:443")
    const fetchFailed = new Error("fetch failed", { cause })
    deps.postJson = async () => {
      throw fetchFailed
    }
    const err = (await runScreenReview({ images: ["/a.png"] }, deps).catch((e: unknown) => e as Error)) as Error
    expect(err.message).toContain("the Gemini Decisions API is unreachable")
    expect(err.message).toContain("/a.png")
    expect(err.message).toContain(DECISIONS_URL)
    expect(err.message).toContain("ECONNREFUSED")
  })

  test("onProgress вызывается после каждого скрина с атрибуцией", async () => {
    const statuses: string[] = []
    const deps = okDeps(["/a.png", "/b.png"])
    deps.onProgress = async (status) => {
      statuses.push(status)
    }
    await runScreenReview({ images: ["/a.png", "/b.png"] }, deps)
    expect(statuses).toEqual([
      "screen_review: 1/2 — a.png done",
      "screen_review: 2/2 — b.png done",
    ])
  })
})

// ——— живой e2e: живой Gemini Decisions API (гвард UWUWU_E2E=1 + ключ) ———
// Главный пользовательский сценарий тула end-to-end: сгенерированный PNG →
// чтение с диска → живой POST /v1beta/decisions → parseAnswers (контракт) →
// markdown-отчёт. Не зависит от внешних файлов-эталонов (пробы 20.11:
// контракт держится и на 1x1 PNG — модель отвечает полным форматом на все
// вопросы; калибровка вероятностей — не предмет e2e, её не проверяем).
// Гвард по культуре репо (прецедент qlty-review.test.ts): без UWUWU_E2E=1
// или без GEMINI_API_KEY сразу skip — живого API в CI нет; при включённом
// гварде без ключа тест падает честной ошибкой «GEMINI_API_KEY не задан» —
// это контракт тула.

const geminiE2E = process.env.UWUWU_E2E === "1" && Boolean(process.env.GEMINI_API_KEY)

describe("runScreenReview e2e — живой Gemini Decisions API (UWUWU_E2E=1)", () => {
  test.skipIf(!geminiE2E)(
    "скрин через живой /v1beta/decisions → отчёт с полным чек-листом и атрибуцией",
    async () => {
      const { rmSync } = await import("node:fs")
      const { mkdtemp, writeFile } = await import("node:fs/promises")
      const { tmpdir } = await import("node:os")
      const { join } = await import("node:path")

      const dir = await mkdtemp(join(tmpdir(), "screen-review-e2e-"))
      const imagePath = join(dir, "e2e-probe.png")
      try {
        // валидный 1x1 белый PNG
        const png1x1 = Buffer.from(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
          "base64",
        )
        await writeFile(imagePath, png1x1)

        const report = await runScreenReview({ images: [imagePath] })

        // шапка с моделью и дефолтным чек-листом, хвост с ролью скринера
        expect(report).toContain("## screen_review (gemini-decision-1, checklist ui-slop, threshold 0.6)")
        expect(report).toContain("The tool is a screener/aimer, not the final judge")
        // атрибуция к скрину: секция с именем файла
        expect(report).toContain(`### Скрин: e2e-probe.png`)
        // контракт живьём: все 7 вопросов чек-листа в таблице, input_tokens
        for (const id of ["double_frame", "card_in_card", "alignment", "overlap", "contrast", "main_defect", "severity"]) {
          expect(report).toContain(`| ${id} |`)
        }
        expect(report).toMatch(/input_tokens: \d+/)
        // дрейф формата API не маскируется
        expect(report).not.toContain("the Decisions API answer contract is broken")
      } finally {
        rmSync(dir, { recursive: true, force: true })
      }
    },
    60_000,
  )
})
