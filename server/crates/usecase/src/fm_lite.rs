//! Лёгкий разбор markdown-фронтmatterа и ISO-штампов для юзкейсов
//! (fm-модуль инфры недоступен слою — здесь свой минимальный, без regex).

use std::collections::BTreeMap;

pub fn parse(md: &str) -> BTreeMap<String, String> {
    let rest = match md.strip_prefix("---\n") {
        Some(r) => r,
        None => return BTreeMap::new(),
    };
    let Some(end) = rest.find("\n---") else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

pub fn strip(md: &str) -> &str {
    let Some(rest) = md.strip_prefix("---\n") else {
        return md;
    };
    let Some(end) = rest.find("\n---") else {
        return md;
    };
    let body = rest[end + 4..].trim_start_matches('\n');
    // хвостовой сепаратор fm-эпохи: «fm\n---\n---\nконтент» — после вырезки
    // блока остаётся одинокий «---» (закрытие + разделитель старого формата)
    body.strip_prefix("---\n")
        .map(|b| b.trim_start_matches('\n'))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_strip() {
        let md = "---\ntitle: Ночной QA\nschedule: 1d\nagent: tester\n---\n\n## Инструкция\nтекст";
        let fm = parse(md);
        assert_eq!(fm.get("schedule").map(String::as_str), Some("1d"));
        assert_eq!(fm.get("agent").map(String::as_str), Some("tester"));
        assert!(strip(md).starts_with("## Инструкция"));
        assert!(parse("без fm").is_empty());
    }

    #[test]
    fn strip_double_separator() {
        // fm-эпоха: закрытие fm + разделитель тела → одинокий «---»
        let md = "---\nstatus: Draft\n---\n---\n\n## Постановка\n";
        assert_eq!(strip(md), "## Постановка\n");
        // обычный fm
        assert_eq!(strip("---\na: 1\n---\nтекст"), "текст");
        // без fm — как есть
        assert_eq!(strip("просто текст"), "просто текст");
    }
}
