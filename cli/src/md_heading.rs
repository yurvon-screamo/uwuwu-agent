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
    !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_requires_space_after_hashes() {
        assert_eq!(heading_of("#bug"), None);
        assert_eq!(heading_of("##x"), None);
    }

    #[test]
    fn heading_detects_level_and_text() {
        assert_eq!(heading_of("# Титул"), Some((1, "Титул".to_string())));
        assert_eq!(heading_of("### Подсекция"), Some((3, "Подсекция".to_string())));
    }

    #[test]
    fn heading_rejects_seven_hashes_and_empty_text() {
        assert_eq!(heading_of("####### x"), None);
        assert_eq!(heading_of("## "), None);
    }

    #[test]
    fn heading_normalizes_whitespace() {
        assert_eq!(heading_of("##  Двойные   пробелы "), Some((2, "Двойные пробелы".to_string())));
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
        assert_eq!(heading_of("## A ^не id!"), Some((2, "A ^не id!".to_string())));
    }

    #[test]
    fn normalize_argument_trims_and_collapses() {
        assert_eq!(normalize_heading_text("  Постановка  "), "Постановка");
    }

    #[test]
    fn has_h2_section_matches_normalized_heading() {
        assert!(has_h2_section("вступление\n\n##  Комменты \n- строка\n", "Комменты"));
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
