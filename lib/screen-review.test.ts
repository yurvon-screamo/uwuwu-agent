// screen-review.test.ts — юнит-тесты ядра screen_review (bun test, без сети:
// оляма подменяется postJson, ФС — readImageBase64). Контракт ответа
// systemone — константа ANSWER_CONTRACT_FIELDS; нехватка поля = громкая
// ошибка, не пустой отчёт. Живой контракт зафиксирован пробами 13.11.2026
// (questions — map; noul/choice/score; usage.input_tokens).
import { describe, expect, test } from "bun:test"
import {
  ANSWER_CONTRACT_FIELDS,
  buildRequest,
  CHECKLISTS,
  DEFAULT_THRESHOLD,
  parseAnswers,
  problemProbability,
  renderReport,
  renderVerdictSection,
  resolveQuestions,
  scoreVerdictLabel,
} from "./screen-review-core"
import { runScreenReview, type ScreenReviewDeps } from "./screen-review"

// Живой ответ systemone от d1-omni (проба 13.11.2026 на эталоне /download,
// структурно сокращён: 4 вопроса, один каждого типа + второй noul).
const SYSTEMONE_OK = JSON.stringify({
  model: "d1-omni",
  answers: {
    double_frame: { type: "noul", noul: 0.8537415956049682 },
    main_defect: {
      type: "choice",
      choice: "double_frame",
      probabilities: { none: 0.37126927631254203, double_frame: 0.5546253782258994, other: 0.07410534546155863 },
      confidence: 0.3319380673388491,
    },
    severity: {
      type: "score",
      score: 1.2746170364258675,
      legend: { "0": "чисто", "1": "мелкие огрехи", "2": "явные дефекты", "3": "критично" },
      probabilities: { "0": 0.29671816651427574, "1": 0.13194663054558112, "2": 0.5713352029401432, "3": 0.000001 },
    },
  },
  usage: { input_tokens: 7344, output_tokens: 0 },
})

describe("parseAnswers — контракт ответа systemone (анти-дрейф)", () => {
  test("живой ответ парсится: три типа, input_tokens", () => {
    const { answers, inputTokens } = parseAnswers(SYSTEMONE_OK, "test")
    expect(inputTokens).toBe(7344)
    expect(answers.double_frame).toEqual({ type: "noul", noul: 0.8537415956049682 })
    expect(answers.main_defect?.type).toBe("choice")
    expect(answers.severity?.type).toBe("score")
  })

  test.each([
    ["не JSON", "{oops", "не JSON"],
    ["answers отсутствует", JSON.stringify({ usage: { input_tokens: 1 } }), "нет map answers"],
    ["noul без числа", JSON.stringify({ answers: { q: { type: "noul" } } }), "type=noul без числа noul"],
    [
      "choice без probabilities",
      JSON.stringify({ answers: { q: { type: "choice", choice: "a" } } }),
      "type=choice без choice/probabilities",
    ],
    ["score без числа", JSON.stringify({ answers: { q: { type: "score" } } }), "type=score без числа score"],
    [
      "неизвестный type",
      JSON.stringify({ answers: { q: { type: "box", box: [] } } }),
      "неизвестный type=box",
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
})

describe("resolveQuestions — чек-листы и inline-вопросы", () => {
  test("дефолт — встроенный ui-slop, 7 вопросов из постановки", () => {
    const q = resolveQuestions({})
    expect(Object.keys(q).sort()).toEqual(
      ["alignment", "card_in_card", "contrast", "double_frame", "main_defect", "overlap", "severity"].sort(),
    )
  })

  test("неизвестное имя чек-листа → громкая ошибка со списком встроенных", () => {
    expect(() => resolveQuestions({ checklist: "perf" })).toThrow("неизвестный чек-лист «perf»")
  })

  test("inline questions заменяют чек-лист", () => {
    const q = resolveQuestions({ questions: { only: { type: "noul", instructions: "тест" } } })
    expect(Object.keys(q)).toEqual(["only"])
  })

  test("оба источника сразу → громкая ошибка, не молчаливый приоритет", () => {
    expect(() => resolveQuestions({ checklist: "ui-slop", questions: { x: { type: "noul", instructions: "x" } } })).toThrow(
      "оставьте один источник вопросов",
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
})

describe("buildRequest — questions уходят map'ом (не массивом)", () => {
  test("questions — объект с ключами-id", () => {
    const q = resolveQuestions({})
    const req = buildRequest("d1-omni", "state", "B64", q)
    expect(req.model).toBe("d1-omni")
    expect(req.state).toBe("state")
    expect(req.images).toEqual(["B64"])
    expect(Array.isArray(req.questions)).toBe(false)
    expect(req.questions.double_frame?.type).toBe("noul")
  })

  test("ui-slop: choice-критерии содержат none, score-уровни от меньшего к большему", () => {
    const slop = CHECKLISTS["ui-slop"]
    expect(slop.main_defect.criteria).toHaveProperty("none")
    expect(slop.severity.criteria).toEqual(["чисто", "мелкие огрехи", "явные дефекты", "критично"])
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
  const legend = { "0": "чисто", "1": "мелкие огрехи", "2": "явные дефекты", "3": "критично" }

  test("целый score — точный уровень", () => {
    expect(scoreVerdictLabel({ type: "score", score: 2, legend })).toBe("явные дефекты")
  })

  test("дробный score — диапазон «X → Y»", () => {
    expect(scoreVerdictLabel({ type: "score", score: 1.27, legend })).toBe("мелкие огрехи → явные дефекты")
  })

  test("без legend → null", () => {
    expect(scoreVerdictLabel({ type: "score", score: 1 })).toBeNull()
  })
})

describe("renderVerdictSection — таблица и флаги порога", () => {
  const { answers } = parseAnswers(SYSTEMONE_OK, "test")
  const verdict = { imagePath: "/shots/download.png", imageName: "download.png", answers, inputTokens: 7344 }

  test("флаг P>0.6 на double_frame 0.85, без флага на 0.55-критерии", () => {
    const md = renderVerdictSection(verdict, "state", DEFAULT_THRESHOLD)
    expect(md).toContain("### Скрин: download.png")
    expect(md).toContain("/shots/download.png")
    expect(md).toContain("input_tokens: 7344")
    // noul 0.85 > 0.6 — флаг; choice top 0.55 < 0.6 — нет; score 1-P(чисто)=0.70 > 0.6 — флаг
    expect(md).toContain("| double_frame | noul | 0.85 | 0.85 | ⚠️ |")
    expect(md).toContain("| main_defect | choice | double_frame (0.55) · conf 0.33 | 0.55 | — |")
    expect(md).toContain("| severity | score | 1.27 — мелкие огрехи → явные дефекты | 0.7 | ⚠️ |")
    expect(md).toContain("**Сигналы** (P > 0.6)")
    expect(md).toContain("- double_frame — P 0.85")
  })

  test("нет сигналов — явная строка, не тишина", () => {
    const clean = { ...verdict, answers: { q: { type: "noul" as const, noul: 0.1 } } }
    const md = renderVerdictSection(clean, "state", DEFAULT_THRESHOLD)
    expect(md).toContain("**Сигналов нет** — ни один вопрос не превысил порог 0.6.")
  })
})

describe("renderReport — шапка и напоминание роли скринера", () => {
  test("шапка с моделью и чек-листом, хвост с ролью скринера", () => {
    const md = renderReport(["секция"], { model: "d1-omni", checklistName: "ui-slop" }, 0.6)
    expect(md).toContain("## screen_review (d1-omni, чек-лист ui-slop, порог 0.6)")
    expect(md).toContain("скринер/прицел, не финальный судья")
    expect(md).toContain("секция")
  })
})

// ——— оркестрация с подменой сети/ФС ———

const okDeps = (screens: string[]): ScreenReviewDeps => ({
  readImageBase64: async (path) => {
    if (!screens.includes(path)) throw new Error(`enoent: ${path}`)
    return "B64"
  },
  postJson: async () => ({ status: 200, body: SYSTEMONE_OK }),
  config: { ollamaUrl: "http://mock:11434", model: "d1-omni" },
})

describe("runScreenReview — пер-скрин семантика и валидация входа", () => {
  test("два скрина → две секции, атрибуция вердиктов к скрину", async () => {
    const md = await runScreenReview({ images: ["/a.png", "/b.png"] }, okDeps(["/a.png", "/b.png"]))
    expect(md).toContain("### Скрин: a.png")
    expect(md).toContain("### Скрин: b.png")
    expect(md).toContain("Скрины с флагами")
    expect(md).toContain("`a.png`, `b.png`")
  })

  test("пустой images → громкая ошибка", async () => {
    await expect(runScreenReview({ images: [] }, okDeps([]))).rejects.toThrow("непустой массив путей")
  })

  test("несуществующий скрин → громкая ошибка с путём, не тихий пропуск", async () => {
    await expect(runScreenReview({ images: ["/missing.png"] }, okDeps([]))).rejects.toThrow("/missing.png")
  })

  test("ollama 500 → честный текст ошибки с телом ответа", async () => {
    const deps = okDeps(["/a.png"])
    deps.postJson = async () => ({ status: 500, body: '{"error":"model is required"}' })
    await expect(runScreenReview({ images: ["/a.png"] }, deps)).rejects.toThrow("ollama 500")
    await expect(runScreenReview({ images: ["/a.png"] }, deps)).rejects.toThrow("model is required")
  })

  test("inline-вопросы уходят в запрос, отчёт помечен <inline>", async () => {
    const seenRequests: unknown[] = []
    const deps = okDeps(["/a.png"])
    deps.postJson = async (_url, body) => {
      seenRequests.push(body)
      return { status: 200, body: SYSTEMONE_OK }
    }
    const md = await runScreenReview(
      { images: ["/a.png"], questions: { only: { type: "noul", instructions: "тест" } } },
      deps,
    )
    expect(md).toContain("чек-лист <inline>")
    const req = seenRequests[0] as { questions: Record<string, unknown> }
    expect(Object.keys(req.questions)).toEqual(["only"])
  })

  test("кастомный порог передаётся в рендер", async () => {
    const md = await runScreenReview({ images: ["/a.png"], threshold: 0.9 }, okDeps(["/a.png"]))
    expect(md).toContain("порог 0.9")
    // noul 0.85 < 0.9 — флага нет, сводка без скринов
    expect(md).toContain("Скринов с флагами нет.")
  })
})
