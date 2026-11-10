// tests/child-rigs-scan.ts — сканер класса тихого маскирования PR #207 в
// исходниках интеграционных ригов (используется child-rigs-strict-path.test.ts).
// Мок opencode-REST не имеет права сматчить битый прод-URL по суффиксу/
// подстроке/безъякорному regex — живой сервер отвечает 404, риг остаётся
// зелёным. Паттерны класса:
//   url.endsWith("/api/…") / url.includes("/api/…", "/prompt", "/message", …)
//   безъякорный regex /\/api\/session…/ и slice-нормализация pathname.
// Каждый риг с opencode-моком обязан нести регресс-пин strictPathPin
// (фоллбек-защита самого рига — ассерты вердиктов в мастер-тестах).
// SiYuan-маты (url.includes("/api/attr/…") и др.) — другой клиент, путь
// константный, класс разрыва path-контракта там отсутствует по построению
// (скан задачи 20261110080723-apn0088) — в маркеры не входят.

// opencode-корни контракта REST демонов (lib/hub.ts, cron/cron.ts,
// dreamer/dreamer.ts): путь строится как `${base}/api${path}`, поэтому любой
// не-якорный матч по сырому url глотает /api/api/…
const OPENCODE_ROOTS = ["/api/session", "/api/model"] as const
// bare-суффиксы того же контракта: include/endsWith по сырому url матчит их
// на любом префиксе
const OPENCODE_SUFFIXES = ["/prompt", "/message", "/inbox", "/compact", "/session", "/model"] as const

interface Rule {
  needle: string
  /** якорная/разрешающая форма: при её присутствии в строке правило молчит */
  unless?: string
  message: string
}

const escaped = (root: string): string => root.replaceAll("/", "\\/")

const CODE_RULES: Rule[] = [
  ...OPENCODE_ROOTS.flatMap((root): Rule[] => [
    { needle: `url.includes("${root}`, message: `includes на сыром url для ${root}` },
    { needle: `url.endsWith("${root}`, message: `endsWith на сыром url для ${root}` },
    { needle: escaped(root), unless: `^${escaped(root)}`, message: `безъякорный regex для ${root}` },
  ]),
  ...OPENCODE_SUFFIXES.flatMap((s): Rule[] => [
    { needle: `url.includes("${s}")`, message: `суффикс-матч ${s} на сыром url` },
    { needle: `url.endsWith("${s}")`, message: `суффикс-endsWith ${s} на сыром url` },
  ]),
  { needle: ".pathname.slice(", message: "slice-нормализация pathname" },
]

/** Строка без хвостового коммента («http://» в литералах не режем — отсечение только по « //»). */
const codeLine = (line: string): string => line.replace(/(^|\s)\/\/.*$/, "")

/** Нарушения класса маскирования PR #207 в исходнике рига. */
export function scanRig(source: string): string[] {
  const violations: string[] = []
  const lines = source.split("\n")
  for (const [i, raw] of lines.entries()) {
    const code = codeLine(raw)
    for (const rule of CODE_RULES) {
      const hit = code.includes(rule.needle)
      const anchored = rule.unless !== undefined && code.includes(rule.unless)
      if (hit && !anchored) violations.push(`${i + 1}: ${rule.message}`)
    }
  }
  const hasOpencodeMock = OPENCODE_ROOTS.some((root) => lines.some((l) => codeLine(l).includes(root)))
  const hasPin = lines.some((l) => codeLine(l).includes("strictPathPin"))
  if (hasOpencodeMock && !hasPin) violations.push("риг с opencode-моком без регресс-пина strictPathPin")
  return violations
}
