//! Текстовые утилиты markdown-экспорта/реимпорта SiYuan: обёртки экспорта,
//! секции, нормализация. Используются перезаписью тел (dispatch) и guard'ом
//! (скелет-сравнение пустышек).

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

/// Секция «## X» в markdown: заменить содержимое, заголовок сохранить.
pub(crate) fn replace_md_section(md: &str, section: &str, content: &str) -> String {
    let mut out = String::new();
    let mut in_section = false;
    let mut replaced = false;
    let needle = format!("## {section}");
    for line in md.lines() {
        if line.starts_with("## ") {
            in_section = line.trim() == needle;
            if in_section {
                out.push_str(&needle);
                out.push('\n');
                out.push_str(content.trim());
                out.push('\n');
                replaced = true;
                continue;
            }
        }
        if !in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("\n## {section}\n{content}\n"));
    }
    out
}

/// Каноническая форма для сравнения контента: пробельные run'ы (включая
/// переводы строк) схлопываются в один пробел, края обрезаются.
pub(crate) fn collapse_whitespace(md: &str) -> String {
    md.split_whitespace().collect::<Vec<_>>().join(" ")
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
    fn strip_export_wrapper_cuts_name_line_after_frontmatter() {
        let md = "---\ntitle: таск\n---\n\n# таск\n\nтело\n";
        assert_eq!(strip_export_wrapper(md, "таск"), "тело\n");
    }

    #[test]
    fn strip_export_wrapper_keeps_body_without_name_prefix() {
        let md = "# другой заголовок\nтекст\n";
        assert_eq!(strip_export_wrapper(md, "таск"), "# другой заголовок\nтекст\n");
    }

    #[test]
    fn replace_md_section_replaces_body_keeps_heading() {
        let md = "## А\nстарое\n\n## Б\nвторое\n";
        let out = replace_md_section(md, "А", "новое содержимое\n");
        assert!(out.contains("## А\nновое содержимое"));
        assert!(out.contains("## Б\nвторое"));
    }

    #[test]
    fn replace_md_section_appends_missing_section() {
        let md = "## А\nпервое\n";
        let out = replace_md_section(md, "Новая", "контент");
        assert!(out.contains("## Новая\nконтент"));
        assert!(out.contains("## А\nпервое"));
    }

    #[test]
    fn collapse_whitespace_merges_line_breaks_and_runs() {
        assert_eq!(collapse_whitespace("a\n\n b  c\n"), "a b c");
        assert_eq!(collapse_whitespace("  "), "");
    }
}
