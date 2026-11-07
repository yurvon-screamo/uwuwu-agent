//! Распознавание заголовков markdown: общий примитив write- и read-путей
//! тел задач (replace-section, thread, ленивая секция комментов).
//!
//! Заголовок ATX: `#{1,6}` + пробел — строки вида `#bug` заголовком не
//! являются. Нормализация текста (trim, схлопывание пробелов, срез хвостового
//! `^block-id`) делает матч устойчивым к двойным пробелам и идентификаторам
//! блоков SiYuan в экспорте; записанная строка заголовка при этом хранится
//! байт-в-байт на стороне вызывающего.

/// (уровень, нормализованный текст) заголовка markdown в строке; None — не заголовок.
pub(crate) fn heading_of(line: &str) -> Option<(usize, String)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 || !line[level..].starts_with(' ') {
        return None;
    }
    let text = normalize_heading_text(&line[level..]);
    if text.is_empty() {
        return None;
    }
    Some((level, text))
}

/// Нормализация текста заголовка/аргумента для матча: пробелы схлопнуты,
/// хвостовой `^block-id` срезан.
pub(crate) fn normalize_heading_text(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.rsplit_once(" ^") {
        Some((text, suffix)) if is_block_id(suffix) => text.to_string(),
        _ => collapsed,
    }
}

/// Суффикс block-id SiYuan: непустой ASCII (буквы, цифры, дефисы).
fn is_block_id(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Нормализация ЗАПРОСА секции (--replace-section): запрос может нести
/// собственный ATX-префикс («## Решение», «###  X ^id») — префикс срезается
/// тем же парсером заголовков. Без этого матч никогда не сходится (хэши
/// остаются в want), а append кладёт в тело мусор «## ### X» — двойной
/// префикс (факт 06.11: заголовок h3 из финального отчёта → not-found
/// append «## ### …» в хвост тела).
pub(crate) fn normalize_heading_query(raw: &str) -> String {
    match heading_of(raw.trim()) {
        Some((_, text)) => text,
        None => normalize_heading_text(raw),
    }
}

/// Строка переключает состояние кодового фенса (открывает/закрывает ```).
pub(crate) fn toggles_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Есть ли в markdown h2-секция с именем: нормализованный матч, фенсы не считаются.
pub(crate) fn has_h2_section(md: &str, name: &str) -> bool {
    let want = normalize_heading_text(name);
    let mut fenced = false;
    for line in md.lines() {
        if toggles_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some((2, text)) = heading_of(line) {
            if text == want {
                return true;
            }
        }
    }
    false
}

/// Первый заголовок с именем НЕ на уровне 2 (фенсы не считаются): тёзка-гард
/// для авто-переноса системной секции — контент с «### Комменты» при живой
/// h2-секции создал бы дубль. Возвращает (уровень, байт-точная строка).
pub(crate) fn heading_twin_outside_h2(md: &str, name: &str) -> Option<(usize, String)> {
    let want = normalize_heading_text(name);
    let mut fenced = false;
    for line in md.lines() {
        if toggles_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some((level, text)) = heading_of(line) {
            if level != 2 && text == want {
                return Some((level, line.to_string()));
            }
        }
    }
    None
}

/// Содержимое h2-секции с именем: заголовок + контент до следующего
/// заголовка уровня ≤ 2 вне кодового фенса (или до конца текста). None —
/// секции нет. Строки хранятся дословно: комменты переносятся байт-в-байт
/// (двойные пробелы и ^block-id живы).
pub(crate) fn extract_h2_section(md: &str, name: &str) -> Option<String> {
    let want = normalize_heading_text(name);
    let mut section: Option<Vec<String>> = None;
    let mut fenced = false;
    for line in md.lines() {
        if toggles_fence(line) {
            fenced = !fenced;
            if let Some(lines) = section.as_mut() {
                lines.push(line.to_string());
            }
            continue;
        }
        if let Some((level, text)) = heading_of(line).filter(|_| !fenced) {
            if level <= 2 {
                if section.is_some() {
                    break;
                }
                if level == 2 && text == want {
                    section = Some(vec![line.to_string()]);
                    continue;
                }
            }
        }
        if let Some(lines) = section.as_mut() {
            lines.push(line.to_string());
        }
    }
    section.map(|lines| lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_requires_space_after_hashes() {
        assert_eq!(heading_of("#bug"), None);
        assert_eq!(heading_of("##x"), None);
    }

    #[test]
    fn extract_h2_section_cuts_at_next_heading() {
        let md = "## Комменты\n- один\n  - один-под\n\n- два ^abc123\n## След\nхвост\n";
        let section = extract_h2_section(md, "Комменты").unwrap();
        assert!(section.starts_with("## Комменты\n"));
        assert!(section.contains("- один-под"));
        assert!(section.contains("- два ^abc123"));
        assert!(!section.contains("## След"));
        assert!(!section.contains("хвост"));
    }

    #[test]
    fn extract_h2_section_keeps_h3_and_fence_markers_inside() {
        let md = "## Комменты\n### чей-то заголовок в секции\n```\n## не заголовок — фенс\n```\nхвост секции\n## Граница\n";
        let section = extract_h2_section(md, "Комменты").unwrap();
        assert!(section.contains("### чей-то заголовок"));
        assert!(section.contains("## не заголовок — фенс"));
        assert!(section.contains("хвост секции"));
        assert!(!section.contains("## Граница"));
    }

    #[test]
    fn extract_h2_section_skips_earlier_headings_and_returns_none_when_absent() {
        assert!(extract_h2_section("## А\nтекст\n## Б\nещё\n", "Комменты").is_none());
        assert!(extract_h2_section("### Комменты\nне h2\n", "Комменты").is_none());
        let later = extract_h2_section("## А\nдо\n\n## Комменты\nпосле\n", "Комменты").unwrap();
        assert!(later.starts_with("## Комменты\nпосле\n"));
    }

    #[test]
    fn heading_twin_outside_h2_finds_other_levels_only() {
        let md = "ввод\n### Комменты\nдетали\n";
        assert_eq!(
            heading_twin_outside_h2(md, "Комменты"),
            Some((3, "### Комменты".to_string()))
        );
        let with_h2 = "## Комменты\nтело\n### Комменты\nхвост\n";
        assert_eq!(
            heading_twin_outside_h2(with_h2, "Комменты"),
            Some((3, "### Комменты".to_string()))
        );
        assert_eq!(
            heading_twin_outside_h2("## Комменты\nок\n", "Комменты"),
            None
        );
        // тёзка в фенсе не заголовок
        assert_eq!(
            heading_twin_outside_h2("```\n### Комменты\n```\n", "Комменты"),
            None
        );
    }

    #[test]
    fn heading_detects_level_and_text() {
        assert_eq!(heading_of("# Титул"), Some((1, "Титул".to_string())));
        assert_eq!(
            heading_of("### Подсекция"),
            Some((3, "Подсекция".to_string()))
        );
    }

    #[test]
    fn heading_rejects_seven_hashes_and_empty_text() {
        assert_eq!(heading_of("####### x"), None);
        assert_eq!(heading_of("## "), None);
    }

    #[test]
    fn heading_normalizes_whitespace() {
        assert_eq!(
            heading_of("##  Двойные   пробелы "),
            Some((2, "Двойные пробелы".to_string()))
        );
    }

    #[test]
    fn heading_strips_block_id_suffix() {
        assert_eq!(
            heading_of("## Причина ^20261104120000-ab12cd"),
            Some((2, "Причина".to_string()))
        );
    }

    #[test]
    fn heading_keeps_caret_text_that_is_not_block_id() {
        assert_eq!(
            heading_of("## A ^не id!"),
            Some((2, "A ^не id!".to_string()))
        );
    }

    #[test]
    fn normalize_argument_trims_and_collapses() {
        assert_eq!(normalize_heading_text("  Постановка  "), "Постановка");
    }

    #[test]
    fn normalize_query_strips_atx_prefix() {
        assert_eq!(normalize_heading_query("## Решение"), "Решение");
        assert_eq!(
            normalize_heading_query("###  Подсекция ^20261105230000-ab12cd"),
            "Подсекция"
        );
    }

    #[test]
    fn normalize_query_keeps_plain_name_and_hashless_word() {
        assert_eq!(normalize_heading_query(" Решение "), "Решение");
        assert_eq!(
            normalize_heading_query("#bug"),
            "#bug",
            "нет пробела после хэшей — не ATX, не срезаем"
        );
    }

    #[test]
    fn has_h2_section_matches_normalized_heading() {
        assert!(has_h2_section(
            "вступление\n\n##  Комменты \n- строка\n",
            "Комменты"
        ));
    }

    #[test]
    fn has_h2_section_ignores_fenced_heading() {
        assert!(!has_h2_section("```\n## Комменты\n```", "Комменты"));
    }

    #[test]
    fn has_h2_section_ignores_other_levels() {
        assert!(!has_h2_section("### Комменты\n", "Комменты"));
    }
}
