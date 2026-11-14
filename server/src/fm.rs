//! fm-зеркало тела задачи: frontmatter `---`…`---` в начале issue body.
//! Ручной парсинг строк (без regex): формат плоский `key: value`.
//! Инвариант канона: fm ≡ поля канбана (писатели синхронизируют оба носителя).

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub fields: BTreeMap<String, String>,
}

impl Frontmatter {
    /// Парсит fm из тела; тело без fm (легаси/черновик владельца) — пустой fm.
    pub fn parse(body: &str) -> Self {
        let Some(inner) = fm_block(body) else {
            return Self::default();
        };
        let mut fields = BTreeMap::new();
        for line in inner.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                fields.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Self { fields }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(|s| s.as_str())
    }

    /// Тело без fm-блока (контент задач).
    // write-path статуса/автодона — потребитель приезжает срезом 1b
    // (мутации issue body + fm-пересборка); тесты контракта уже здесь
    #[allow(dead_code)]
    pub fn strip(body: &str) -> &str {
        match fm_bounds(body) {
            Some((_, end)) => body[end..].trim_start_matches('\n'),
            None => body,
        }
    }

    /// Пересобирает тело с новым fm (контент сохраняется дословно).
    #[allow(dead_code)] // write-path среза 1b, см. strip
    pub fn render(body: &str, fields: &BTreeMap<String, String>) -> String {
        let content = Self::strip(body);
        let mut fm = String::from("---\n");
        for (k, v) in fields {
            fm.push_str(&format!("{k}: {v}\n"));
        }
        fm.push_str("---\n\n");
        fm.push_str(content);
        fm
    }
}

fn fm_bounds(body: &str) -> Option<(usize, usize)> {
    let rest = body.strip_prefix("---\n")?;
    let marker = rest.find("\n---")?;
    let after = &rest[marker + 4..];
    // закрывающий маркер — конец строки: `\n---\n` или `\n---` в конце тела
    if !after.is_empty() && !after.starts_with('\n') && !after.starts_with('\r') {
        return None;
    }
    // (конец внутренностей, конец закрывающего маркера) — оба в координатах тела
    Some((4 + marker, 4 + marker + 4))
}

fn fm_block(body: &str) -> Option<&str> {
    let (inner_end, _) = fm_bounds(body)?;
    Some(&body[4..inner_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str =
        "---\nstatus: In Progress\npr: https://x/y/pull/1\n---\n\n## Постановка\n\nТекст.";

    #[test]
    fn parses_known_fields() {
        let fm = Frontmatter::parse(BODY);
        assert_eq!(fm.get("status"), Some("In Progress"));
        assert_eq!(fm.get("pr"), Some("https://x/y/pull/1"));
        assert_eq!(fm.get("branch"), None);
    }

    #[test]
    fn empty_fm_without_block() {
        let fm = Frontmatter::parse("## Постановка\n\nБез fm.");
        assert!(fm.fields.is_empty());
        assert_eq!(Frontmatter::strip("## X"), "## X");
    }

    #[test]
    fn strip_keeps_content() {
        assert_eq!(Frontmatter::strip(BODY), "## Постановка\n\nТекст.");
    }

    #[test]
    fn render_round_trip() {
        let fm = Frontmatter::parse(BODY);
        let mut fields = fm.fields.clone();
        fields.insert("status".into(), "Review".into());
        let rendered = Frontmatter::render(BODY, &fields);
        assert_eq!(Frontmatter::parse(&rendered).get("status"), Some("Review"));
        assert_eq!(Frontmatter::strip(&rendered), "## Постановка\n\nТекст.");
    }

    #[test]
    fn horizontal_rule_is_not_fm() {
        // `---` как разделитель внутри контента (не в начале) — не fm
        let body = "Текст\n\n---\n\nЕщё текст";
        assert!(Frontmatter::parse(body).fields.is_empty());
    }
}
