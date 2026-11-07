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
            let tight = |c: Option<char>| matches!(c, Some(c) if !c.is_alphanumeric() && !c.is_whitespace());
            if tight(prev) && tight(next) {
                count += 1;
            }
        }
    }
    count
}

fn hash_run_warnings(md: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
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

fn duplicate_phrase_warnings(md: &str) -> Vec<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut first_seen: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
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
