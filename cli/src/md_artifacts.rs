//! Артефакты round-trip SiYuan в markdown-выходе: санитайзер zero-width на
//! read-пути, детектор остаточных артефактов (hash-раны тег-мусора,
//! дословные дубли фраз) и write-side превенция тег-матча ядра.
//! Локализация слоя порчи — задача 20261106180646; upstream-класс —
//! siyuan-note/siyuan#19052: фикс Lute 2d14fe5 (milestone 3.8.3) вернул
//! punct-флаги делимитера для `#`, но «»/„“-обёртки с голой решёткой
//! остаются непокрытыми (пробы на ядре 3.8.10-alpha.5).

use crate::md_heading::toggles_fence;

const ZWSP: char = '\u{200B}';
const ZWNJ: char = '\u{200C}';
const ZWJ: char = '\u{200D}';
const BOM: char = '\u{FEFF}';

/// Минимум «голых» решёток с пунктуацией вплотную в одном параграфе,
/// при котором write-side превенция предупреждает.
const TAG_TRAP_MIN_HASHES: usize = 2;

/// Минимальная длина нормализованного сегмента для детекции дословных
/// дублей фраз (короче — шум нормализации, не фраза).
const DUP_PHRASE_MIN_CHARS: usize = 20;

fn is_zero_width(ch: char) -> bool {
    matches!(ch, ZWSP | ZWNJ | ZWJ | BOM)
}

/// Снять zero-width-маркеры ядра вне code-fence'ов: U+200B/ZWSP, U+200C/ZWNJ,
/// U+FEFF/BOM — безусловно; U+200D/ZWJ — только при соседстве с бэктиком или
/// другим zero-width (сигнатура код-спан-маркера), emoji-ZWJ-последовательности
/// (ZWJ между пиктограммами) не ломаются. Содержимое фенсов дословно —
/// форензика-цитаты с нулевой шириной переживают только внутри фенсов.
pub(crate) fn strip_zero_width(md: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            lines.push(line.to_string());
            continue;
        }
        if in_fence {
            lines.push(line.to_string());
            continue;
        }
        lines.push(strip_line_zero_width(line));
    }
    let mut stripped = lines.join("\n");
    if md.ends_with('\n') && !stripped.ends_with('\n') {
        stripped.push('\n');
    }
    stripped
}

fn strip_line_zero_width(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    for (i, ch) in chars.iter().copied().enumerate() {
        match ch {
            ZWSP | ZWNJ | BOM => {}
            ZWJ => {
                let prev = i.checked_sub(1).map(|j| chars[j]);
                let next = chars.get(i + 1).copied();
                let marker_adjacent = [prev, next]
                    .into_iter()
                    .flatten()
                    .any(|n| n == '`' || is_zero_width(n));
                if !marker_adjacent {
                    out.push(ch);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Детектор остаточных артефактов round-trip на read-выходе (после
/// санитайзера): hash-раны `# # #` вне фенсов и заголовков (визуальный
/// след тег-мусора) и дословные дубли фраз. Возможные находки — не ремонт:
/// хранение уже несёт порчу, отчёт для читателя и ревью.
pub(crate) fn artifact_report(md: &str) -> Vec<String> {
    let mut report = hash_run_warnings(md);
    report.extend(duplicate_phrase_warnings(md));
    report
}

/// Write-side превенция тег-матча: ≥2 «голых» решёток с пунктуацией
/// вплотную с обеих сторон в одном параграфе (сигнатура `«#»`/`„#“`,
/// пробированный триггер поедания бэктиков на экспорте). Предупреждение,
/// не блок: легитимный контент о самой порче (тела задач о round-trip)
/// триггерить может — решение пишет агент, видя причину.
pub(crate) fn tag_trap_warning(md: &str) -> Option<String> {
    for para in paragraphs_outside_fences(md) {
        let hashes = tight_hash_count(&para);
        if hashes >= TAG_TRAP_MIN_HASHES {
            return Some(format!(
                "⚠ возможный тег-матч SiYuan: решётка с пунктуацией вплотную с обеих сторон \
                 (сигнатура «#») встречается в параграфе {hashes} раз — на экспорте ядро может \
                 съесть бэктики код-спанов между ними. Безопасные формы: пробелы вокруг \
                 решётки, число сразу после («#83»), решётка внутри код-спана"
            ));
        }
    }
    None
}

/// Параграфы (блоки, разделённые пустыми строками) с fence-трекингом:
/// пустая строка внутри фенса параграф не рвёт.
fn paragraphs_outside_fences(md: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            current.push(line);
            continue;
        }
        if !in_fence && line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
            continue;
        }
        current.push(line);
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }
    paragraphs
}

fn tight_hash_count(md: &str) -> usize {
    let mut in_fence = false;
    let mut count = 0;
    for line in md.lines() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for (i, ch) in chars.iter().copied().enumerate() {
            if ch != '#' {
                continue;
            }
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            // решётка на границе инлайн-кода (`#`) — safe-форма из проб,
            // сам warning советует её; тег-матч на ней не стреляет
            if prev == Some('`') || next == Some('`') {
                continue;
            }
            let tight = |c: Option<char>| matches!(c, Some(c) if !c.is_alphanumeric() && !c.is_whitespace());
            if tight(prev) && tight(next) {
                count += 1;
            }
        }
    }
    count
}

/// Строки контента для сканеров: frontmatter-блок в начале — структурная
/// обёртка экспорта (title/date/lastmod), не контент; без её среза
/// `title:`-строка дублирует титульную H1 и даёт систематический ложный
/// «дословный дубль фразы» на каждом длиннотайтловом доке (ревью-раунд 5).
fn content_lines(md: &str) -> Vec<&str> {
    let mut rest = md;
    if md.starts_with("---\n") {
        if let Some((_, after)) = md.split_once("\n---") {
            rest = after.trim_start_matches('\n');
        }
    }
    rest.lines().collect()
}

fn hash_run_warnings(md: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut in_fence = false;
    for line in content_lines(md) {
        if toggles_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(run) = max_hash_run(line) {
            warnings.push(format!(
                "hash-ран из {run} решёток через пробел вне фенса/заголовка — след тег-мусора \
                 round-trip: {}",
                clip(line, 80)
            ));
        }
    }
    warnings
}

/// Длина максимального рана `#`-символов, разделённых одиночными пробелами.
fn max_hash_run(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let mut best = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' {
            let mut run = 1;
            let mut j = i + 1;
            while j + 1 < chars.len() && chars[j] == ' ' && chars[j + 1] == '#' {
                run += 1;
                j += 2;
            }
            best = best.max(run);
            i = j;
        } else {
            i += 1;
        }
    }
    (best >= 3).then_some(best)
}

/// Title из frontmatter-блока экспорта (`title:` — первая строка блока).
/// None — frontmatter нет или без title: матч экспорт-препенд H1 не
/// выполняется, поведение скана прежнее.
fn frontmatter_title(md: &str) -> Option<&str> {
    let rest = md.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    let title_line = frontmatter.lines().next()?;
    title_line.strip_prefix("title:").map(str::trim)
}

/// Индекс первой непустой контент-строки, если она — H1, дословно равная
/// title (collapse-нормализация обеих сторон). Сигнатура экспорт-препенда:
/// create с ведущим `# <тайтл>` в payload даёт в экспорте две одинаковые
/// H1 подряд (ядерный prepend + payload) — генезис write-path, не
/// round-trip порча (кейсы ADR-063 и cicd-док, 08.11). Осознанный
/// trade-off: дубль титульной H1 детектором больше не флагается — живой
/// класс round-trip порчи такой сигнатуры не показан.
fn h1_prepend_index(lines: &[&str], title: Option<&str>) -> Option<usize> {
    let title = title?;
    let prepend_h1 = format!("# {}", crate::markdown::collapse_whitespace(title));
    let index = lines.iter().position(|line| !line.trim().is_empty())?;
    let first = crate::markdown::collapse_whitespace(lines[index].trim());
    (first == prepend_h1).then_some(index)
}

/// Строка живого HTML-блока: экспорт ядра отдаёт блок одной строкой,
/// начинающейся с открывающего (`<div …`) или закрывающего (`</div …`)
/// тега. Дубли inline-CSS/разметки между диаграммами rich-доков —
/// закономерная структура, не round-trip (кейс «Стек и архитектура»:
/// все предупреждения на чтении — от CSS-фрагментов внутри html-строк).
fn is_html_line(line: &str) -> bool {
    let Some(after_open) = line.trim_start().strip_prefix('<') else {
        return false;
    };
    match after_open.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => true,
        Some('/') => true,
        _ => false,
    }
}

/// Строки контента для duplicate-скана после precision-скипов: frontmatter
/// (content_lines), фенсы (маркеры и содержимое), секция «## Комменты»
/// (повторы вердиктов между раундами ревью — закономерный контент, до
/// следующего ##-заголовка), html-строки живых блоков (дубли inline-CSS
/// диаграмм — закономерная разметка) и экспорт-препенд H1 (генезис create
/// с ведущим `# <тайтл>`, не round-trip порча).
fn duplicate_scan_lines<'a>(lines: &[&'a str], skip_h1: Option<usize>) -> Vec<&'a str> {
    let mut scanned = Vec::new();
    let mut in_fence = false;
    let mut in_comments = false;
    for (index, line) in lines.iter().copied().enumerate() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || Some(index) == skip_h1 {
            continue;
        }
        let trimmed = line.trim();
        if trimmed == "## Комменты" {
            in_comments = true;
            continue;
        }
        if in_comments {
            // секция закрыта следующим ##-заголовком — контент после неё
            // снова сканируется
            if trimmed.starts_with("## ") {
                in_comments = false;
            } else {
                continue;
            }
        }
        if is_html_line(line) {
            continue;
        }
        scanned.push(line);
    }
    scanned
}

fn duplicate_phrase_warnings(md: &str) -> Vec<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut first_seen: Vec<String> = Vec::new();
    let lines = content_lines(md);
    let skip_h1 = h1_prepend_index(&lines, frontmatter_title(md));
    for line in duplicate_scan_lines(&lines, skip_h1) {
        let normalized = crate::markdown::collapse_whitespace(line);
        for segment in normalized.split([',', ';', '.', '!', '?', '…']) {
            let phrase = segment.trim();
            if phrase.chars().count() < DUP_PHRASE_MIN_CHARS {
                continue;
            }
            let count = counts.entry(phrase.to_string()).or_insert(0);
            if *count == 0 {
                first_seen.push(phrase.to_string());
            }
            *count += 1;
        }
    }
    first_seen
        .into_iter()
        .filter(|phrase| counts[phrase] >= 2)
        .map(|phrase| format!("дословный дубль фразы: «{}»", clip(&phrase, 60)))
        .collect()
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZWSP_LINE: &str = "текст \u{200b}`code`\u{200b} хвост";

    // ——— strip_zero_width: поведение read-санитайзера ———

    #[test]
    fn strips_zwsp_zwnj_bom_outside_fences() {
        let dirty = "текст \u{200b}`code` \u{200c}между\u{feff} конец\n";
        assert_eq!(strip_zero_width(dirty), "текст `code` между конец\n");
    }

    #[test]
    fn strips_zwj_adjacent_to_backtick_keeps_emoji_sequence() {
        let with_emoji = "жирный \u{200d}`x` и \u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467} семья\n";
        let clean = strip_zero_width(with_emoji);
        assert_eq!(
            clean,
            "жирный `x` и \u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467} семья\n"
        );
    }

    #[test]
    fn strips_zwj_next_to_zero_width_sibling() {
        let paired = "текст \u{200b}\u{200d}`x` хвост\n";
        assert_eq!(strip_zero_width(paired), "текст `x` хвост\n");
    }

    #[test]
    fn keeps_fence_content_verbatim() {
        let dirty = "до \u{200b}`\n```\nkeep \u{200b}\u{200d} inside\n```\nпосле \u{feff}\n";
        let clean = strip_zero_width(dirty);
        assert_eq!(
            clean,
            "до `\n```\nkeep \u{200b}\u{200d} inside\n```\nпосле \n"
        );
    }

    #[test]
    fn preserves_trailing_newline_state() {
        assert_eq!(strip_zero_width(ZWSP_LINE), "текст `code` хвост");
        assert_eq!(
            strip_zero_width(&format!("{ZWSP_LINE}\n")),
            "текст `code` хвост\n"
        );
    }

    #[test]
    fn strip_zero_width_is_idempotent() {
        let dirty =
            "текст \u{200b}`code` \u{200d}хвост \u{200c}\u{feff}и \u{1F469}\u{200D}\u{1F469}\n";
        let once = strip_zero_width(dirty);
        assert_eq!(strip_zero_width(&once), once);
    }

    #[test]
    fn inline_code_hashes_are_safe_from_trap_warning() {
        let code_spans = "маркер `#` и код `y` между `#` и `#` же.\n";
        assert!(tag_trap_warning(code_spans).is_none());
    }

    #[test]
    fn frontmatter_title_pair_with_h1_stays_silent() {
        let noisy = "---\ntitle: SiYuan write-path: round-trip калечит markdown тел — дубли фраз, \
                     съедение пустых строк — локализовать слой\n---\n\n# SiYuan write-path: \
                     round-trip калечит markdown тел — дубли фраз, съедение пустых строк — \
                     локализовать слой\n\nштатное тело\n";
        assert!(artifact_report(noisy).is_empty());
    }

    #[test]
    fn frontmatter_dropped_without_shadowing_real_duplicates() {
        let noisy = "---\ntitle: какой-то тайтл задачи\n---\n\nформулировка проверки задачи \
                     полностью уникальная, формулировка проверки задачи полностью уникальная\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("дословный дубль фразы"));
    }

    #[test]
    fn clean_markdown_passes_unchanged() {
        let clean = "# План\n\nшаг один\n\n- пункт `код` и **жирный**\n";
        assert_eq!(strip_zero_width(clean), clean);
    }

    // ——— artifact_report: сигнатуры остаточной порчи ———

    #[test]
    fn reports_hash_runs_outside_headings_only() {
        let noisy = "## Заголовок # # #\n\nмусор: # # # # #\n\n| # | # | # |\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("hash-ран"));
        assert!(report[0].contains("мусор"));
    }

    #[test]
    fn short_hash_pairs_and_single_hashes_stay_silent() {
        let fine = "маркер «#» и #83 в строке\n\nref: # #\n";
        assert!(artifact_report(fine).is_empty());
    }

    #[test]
    fn reports_duplicate_phrases_across_lines() {
        let noisy = "байтовый diff секции до/после — артефакт в scratch, байтовый diff \
                     секции до/после — артефакт в scratch\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("дословный дубль фразы"));
        assert!(report[0].contains("байтовый diff"));
    }

    #[test]
    fn unique_phrases_and_short_segments_stay_silent() {
        let fine = "первая уникальная формулировка задачи, вторая уникальная формулировка \
                    цели проверки (3.8.10)\n";
        assert!(artifact_report(fine).is_empty());
    }

    #[test]
    fn duplicate_detection_skips_fences() {
        let noisy = "```\nшаблон отчёта по контракту документации\n```\n\nшаблон отчёта по \
                     контракту документации\n";
        assert!(artifact_report(noisy).is_empty());
    }

    #[test]
    fn comments_section_duplicates_stay_silent() {
        let noisy = "штатное тело с дублем фразы целиком, штатное тело с дублем фразы целиком\n\
                     \n\
                     ## Комменты\n\
                     \n\
                     - **AGENTS**: Вердикт: request_changes — доработать срез плана целиком\n\
                     \n\
                     - **AGENTS**: Вердикт: request_changes — доработать срез плана целиком\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("штатное тело"));
    }

    #[test]
    fn comments_section_resumes_after_next_heading() {
        let noisy = "## Комменты\n\
                     \n\
                     - **AGENTS**: повтор вердикта ревью внутри секции комментов задачи\n\
                     \n\
                     - **AGENTS**: повтор вердикта ревью внутри секции комментов задачи\n\
                     \n\
                     ## Проверка\n\
                     \n\
                     живой дубль после закрытия секции комментов, живой дубль после закрытия \
                     секции комментов\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("живой дубль"));
    }

    #[test]
    fn html_block_style_duplicates_stay_silent() {
        let noisy = "<div style=\"border:1px solid #2E2E2E;font-size:11px;color:#ffffff\">\
                     Акторы</div>\n\
                     \n\
                     <div style=\"border:1px solid #2E2E2E;font-size:11px;color:#ffffff\">\
                     Платформа</div>\n\
                     \n\
                     текстовый дубль вне диаграмм ловится, текстовый дубль вне диаграмм ловится\n";
        let report = artifact_report(noisy);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("текстовый дубль"));
    }

    #[test]
    fn html_line_predicate_matches_open_and_close_tags() {
        assert!(is_html_line("<div style=\"color:#fff\">x</div>"));
        assert!(is_html_line("  </div>"));
        assert!(!is_html_line("просто текст со <тегом> в середине"));
        assert!(!is_html_line("неравенство: a < b > c"));
    }

    #[test]
    fn export_prepended_h1_pair_stays_silent() {
        let noisy = "---\ntitle: ADR-063: CI-гейт на PR — required check вместо ручных прогонов\n\
                     ---\n\
                     \n\
                     # ADR-063: CI-гейт на PR — required check вместо ручных прогонов\n\
                     \n\
                     # ADR-063: CI-гейт на PR — required check вместо ручных прогонов\n\
                     \n\
                     ## Статус\n\
                     \n\
                     Accepted\n";
        assert!(artifact_report(noisy).is_empty());
    }

    #[test]
    fn h1_duplicate_after_body_text_stays_reported() {
        let noisy = "вводный параграф до заголовков, вводный параграф до заголовков\n\
                     \n\
                     # длинный тайтл без режущей пунктуации из двадцати знаков\n\
                     \n\
                     # длинный тайтл без режущей пунктуации из двадцати знаков\n";
        let report = artifact_report(noisy);
        assert!(report.iter().any(|w| w.contains("длинный тайтл")));
    }

    #[test]
    fn nested_task_get_header_duplicates_survive() {
        let title = "какой-то тайтл задачи длиннее двадцати символов";
        let header = format!(
            "Задача:   {title}  \nСтатус:   Design  \nСтрока:   20261108003111-q0m3974  \n"
        );
        let noisy = format!(
            "---\ntitle: {title}\n---\n\n# {title}\n\n{header}\n\n----- тело -----\n\n\
             {header}\n\n## Постановка\nштатное тело\n"
        );
        let report = artifact_report(&noisy);
        assert!(report.iter().any(|w| w.contains("Задача:")));
    }

    // ——— tag_trap_warning: write-side превенция тег-матча ———

    #[test]
    fn warns_on_two_bare_hashes_in_quote_wrappers() {
        let damaging = "маркер «#» и код `bare_guille` между «#» и «#» же.\n";
        let warning = tag_trap_warning(damaging).expect("повреждающая форма должна кричать");
        assert!(warning.contains("тег-матч"));
    }

    #[test]
    fn single_trap_hash_stays_silent() {
        let lone = "одинокий маркер «#» с кодом `single` в строке.\n";
        assert!(tag_trap_warning(lone).is_none());
    }

    #[test]
    fn numeric_and_spaced_hashes_are_safe_forms() {
        let safe = "закрыт (`paren_code`, Routine) по «#83» и «#84» конвенции, ref # 85.\n";
        assert!(tag_trap_warning(safe).is_none());
    }

    #[test]
    fn trap_hashes_in_different_paragraphs_are_separate() {
        let split = "маркер «#» первый.\n\nкод `safe_code` между «#» и хвост.\n";
        assert!(tag_trap_warning(split).is_none());
    }

    #[test]
    fn fences_are_excluded_from_trap_count() {
        let fenced = "маркер «#»\n\n```\n«#» «#» «#»\n```\nхвост.\n";
        assert!(tag_trap_warning(fenced).is_none());
    }

    #[test]
    fn clip_truncates_long_text_with_ellipsis() {
        let long = "а".repeat(100);
        assert_eq!(clip(&long, 60).chars().count(), 61);
    }
}
