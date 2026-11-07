//! Текстовые утилиты markdown-экспорта/реимпорта SiYuan: обёртки экспорта
//! и нормализация. Используются перезаписью тел (dispatch) и guard'ом
//! (скелет-сравнение пустышек); секционная логика живёт в dispatch
//! (поверх примитивов md_heading).

use crate::md_heading::toggles_fence;

/// Снять экспортную обёртку дока: YAML frontmatter и рендер-строку имени
/// (`# <имя дока>` первой строкой поверх блоков — она не блок; без strip
/// реимпортируется реальным heading-блоком и множится на каждый rewrite).
pub(crate) fn strip_export_wrapper(md: &str, doc_title: &str) -> String {
    let body = strip_frontmatter(md).trim_start().to_string();
    match body.strip_prefix(&format!("# {doc_title}")) {
        Some(rest) => rest.trim_start_matches('\n').to_string(),
        None => body,
    }
}

/// YAML frontmatter из начала markdown (--- ... ---) — live-атрибуты дока.
pub(crate) fn strip_frontmatter(md: &str) -> String {
    let rest = match md.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return md.to_string(),
    };
    match rest.split_once("\n---") {
        Some((_, after)) => after.trim_start_matches('\n').to_string(),
        None => md.to_string(),
    }
}

/// Каноническая форма для сравнения контента: пробельные run'ы (включая
/// переводы строк) схлопываются в один пробел, края обрезаются.
pub(crate) fn collapse_whitespace(md: &str) -> String {
    md.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Строки текста в хвост-эквивалентной нормализации (порт _normalized_lines
/// из tools/wiki_ops.py, контракт живых проб ядра): гасит рендер-шум
/// round-trip без слипания семантических различий. Вне code-fence: пустые
/// строки — дроп (ядро разыхивает списки, вставляя их); внутренние
/// whitespace-разбеги — схлоп до одного пробела; ведущий отступ строки —
/// дословно (слипание «без отступа»/«с отступом» дало бы тихую потерю
/// сравнения: top-level vs подсписок). Строки таблиц (≥2 pipe): whitespace
/// вплотную к pipe — дроп (ядро стирает его при roundtrip); separator-строка
/// (только pipe/дефисы/двоеточия, есть дефис) — канонизация: ядро пэддит
/// дефисы до ширины колонок, скелет двоеточий выравнивания сохраняется.
/// Внутри code-fence — дословно: пробелы кода семантичны. Fence-маркер —
/// тройные бэктики (как в python-источнике; ~~~ фенсом сравнения не считается).
/// Границы осознанные: дроп пустых строк вне fence рендер-различим для
/// tight/loose списков — это слипание вариантов одного текста для детекции
/// ретрая, не «чинить».
pub(crate) fn normalized_lines(text: &str) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if toggles_fence(line) {
            in_fence = !in_fence;
            normalized.push(line.trim_end().to_string());
            continue;
        }
        if in_fence {
            normalized.push(line.to_string());
            continue;
        }
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        let leading = &line[..line.len() - line.trim_start().len()];
        if is_separator_row(stripped) {
            normalized.push(format!("{leading}{}", canonical_separator(stripped)));
        } else if stripped.matches('|').count() >= 2 {
            normalized.push(format!("{leading}{}", collapse_table_cells(stripped)));
        } else {
            normalized.push(format!(
                "{leading}{}",
                stripped.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
    }
    normalized
}

/// Separator-строка таблицы: весь состав из whitespace/pipe/двоеточий/дефисов,
/// есть хотя бы один pipe и хотя бы один дефис (порт SEPARATOR_ROW_RE
/// fullmatch: `(?=.*\|)[\s|:\-]*-[\s|:\-]*`).
fn is_separator_row(stripped: &str) -> bool {
    stripped.contains('|')
        && stripped.contains('-')
        && stripped
            .chars()
            .all(|c| c.is_whitespace() || c == '|' || c == ':' || c == '-')
}

/// Separator таблицы → «|» + скелет двоеточий каждой колонки + «-».
/// Пэддинг дефисов ядром нестабилен (ширина колонок), позиции двоеточий —
/// семантика выравнивания: «:---:» → «:-:», «----------» → «-».
fn canonical_separator(row: &str) -> String {
    let cells: Vec<&str> = row.trim().trim_matches('|').split('|').collect();
    let skeleton: Vec<String> = cells
        .iter()
        .map(|cell| {
            let cell = cell.trim();
            if cell.is_empty() {
                return String::new();
            }
            let left = if cell.starts_with(':') { ":" } else { "" };
            let right = if cell.len() > 1 && cell.ends_with(':') {
                ":"
            } else {
                ""
            };
            format!("{left}-{right}")
        })
        .collect();
    format!("|{}|", skeleton.join("|"))
}

/// Строка таблицы: whitespace вплотную к pipe — дроп (сравнение с тем,
/// что ядро отдаёт после roundtrip).
fn collapse_table_cells(stripped: &str) -> String {
    stripped
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_frontmatter_cuts_leading_yaml_block() {
        let md = "---\ntitle: т\ndate: x\n---\n\n# Заголовок\n\nтекст\n";
        assert_eq!(strip_frontmatter(md), "# Заголовок\n\nтекст\n");
    }

    #[test]
    fn strip_frontmatter_keeps_markdown_without_frontmatter() {
        let md = "# Обычный док\n\n---\n\nhr внутри текста\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn strip_frontmatter_keeps_unterminated_block() {
        let md = "---\ntitle: без закрывашки\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn normalized_lines_is_idempotent() {
        let noisy = "текст\n\nсписок:\n  - раз   два\n- три\n\n| a  | b |\n| --- | :---: |\n";
        let once = normalized_lines(noisy);
        assert_eq!(normalized_lines(&once.join("\n")), once);
    }

    #[test]
    fn normalized_lines_collapses_render_noise() {
        // ядро вставляет пустые строки в списки и стирает двойные пробелы
        let before = normalized_lines("пункт  один\n\nпункт два\n");
        let after = normalized_lines("пункт один\nпункт два\n");
        assert_eq!(before, after);
    }

    #[test]
    fn normalized_lines_keeps_fence_content_verbatim() {
        let with_noise = normalized_lines("```\na  b\n\n\n  c\n```\n");
        let clean = normalized_lines("```\na  b\n\n\n  c\n```\n");
        assert_eq!(with_noise, clean);
        assert!(with_noise.contains(&"a  b".to_string()));
        assert!(with_noise.contains(&"  c".to_string()));
    }

    #[test]
    fn normalized_lines_keeps_leading_indent() {
        let indented = normalized_lines("- верх\n  - низ\n");
        assert_eq!(indented, vec!["- верх".to_string(), "  - низ".to_string()]);
    }

    #[test]
    fn normalized_lines_canonicalizes_table_padding_and_separator() {
        // пэддинг дефисов нестабилен: канонизируется до «-» на колонку
        let plain = normalized_lines("| a | b |\n|-----|-----------|\n");
        assert_eq!(plain, normalized_lines("|a|b|\n| - | - |\n"));
        // скелет двоеточий — семантика выравнивания, сохраняется
        let aligned = normalized_lines("| a | b |\n| --- | :---: |\n");
        assert_eq!(aligned, normalized_lines("|a|b|\n|---|:--:|\n"));
        assert_ne!(plain, aligned, "выравнивание различает сепараторы");
    }

    #[test]
    fn normalized_lines_drops_blanks_but_not_inside_fence() {
        let md = "до\n\n```\n\n\nпосле фенса внутри\n```\n\nконец\n";
        let lines = normalized_lines(md);
        assert_eq!(
            lines,
            vec![
                "до".to_string(),
                "```".to_string(),
                String::new(),
                String::new(),
                "после фенса внутри".to_string(),
                "```".to_string(),
                "конец".to_string(),
            ]
        );
    }

    #[test]
    fn strip_export_wrapper_cuts_name_line_after_frontmatter() {
        let md = "---\ntitle: таск\n---\n\n# таск\n\nтело\n";
        assert_eq!(strip_export_wrapper(md, "таск"), "тело\n");
    }

    #[test]
    fn strip_export_wrapper_keeps_body_without_name_prefix() {
        let md = "# другой заголовок\nтекст\n";
        assert_eq!(
            strip_export_wrapper(md, "таск"),
            "# другой заголовок\nтекст\n"
        );
    }

    #[test]
    fn collapse_whitespace_merges_line_breaks_and_runs() {
        assert_eq!(collapse_whitespace("a\n\n b  c\n"), "a b c");
        assert_eq!(collapse_whitespace("  "), "");
    }
}
