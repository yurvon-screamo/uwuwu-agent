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

/// Срезает fm-блок в начале входа. Переносы маркера, терминатора и
/// пустых строк — LF или CRLF (#1664); тело возвращается срезом без
/// переписывания байтов.
pub fn strip(md: &str) -> &str {
    let Some(rest) = md
        .strip_prefix("---\r\n")
        .or_else(|| md.strip_prefix("---\n"))
    else {
        return md;
    };
    // find("\n---") матчит и LF, и CRLF-терминатор: CR остаётся перед
    // найденным индексом, срез начинается сразу после «---».
    let Some(end) = rest.find("\n---") else {
        return md;
    };
    let body = trim_blank_lines(&rest[end + 4..]);
    // хвостовой сепаратор fm-эпохи: «fm\n---\n---\nконтент» — после вырезки
    // блока остаётся одинокий «---» (закрытие + разделитель старого формата)
    body.strip_prefix("---\r\n")
        .or_else(|| body.strip_prefix("---\n"))
        .map(trim_blank_lines)
        .unwrap_or(body)
}

/// Ведущие пустые строки после fm — LF и CRLF одинаково, без потери
/// переносов самого тела.
fn trim_blank_lines(md: &str) -> &str {
    let mut rest = md;
    loop {
        let Some(next) = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
        else {
            return rest;
        };
        rest = next;
    }
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

    /// #1664: CRLF-легаси-fm срезается; переносы тела не переписываются.
    #[test]
    fn strip_crlf_legacy_fm_keeps_body_bytes() {
        let md = "---\r\ntitle: Ночной QA\r\nschedule: 1d\r\n---\r\n\r\n## Инструкция\r\nтекст";
        assert_eq!(strip(md), "## Инструкция\r\nтекст");
    }

    /// #1664: префикс и терминатор допускают разные переносы.
    #[test]
    fn strip_mixed_line_endings() {
        let cases = [
            ("---\r\na: 1\n---\n\n## Тело\n", "## Тело\n"),
            ("---\na: 1\r\n---\r\n\r\n## Тело\r\n", "## Тело\r\n"),
        ];
        for (md, expected) in cases {
            assert_eq!(strip(md), expected, "форма: {md:?}");
        }
    }

    /// #1664: одинокий сепаратор fm-эпохи срезается и в CRLF-форме.
    #[test]
    fn strip_crlf_double_separator() {
        let md = "---\r\nstatus: Draft\r\n---\r\n---\r\n\r\n## Постановка\r\n";
        assert_eq!(strip(md), "## Постановка\r\n");
    }

    /// #1664: без fm вход цел, включая «---» как строку тела.
    #[test]
    fn strip_keeps_crlf_text_without_fm() {
        let md = "## Постановка\r\nтекст\r\n---\r\nещё текст";
        assert_eq!(strip(md), md);
    }

    /// #1664: доки ботов и кронов в CRLF — fm снимается, тело байт-в-байт.
    #[test]
    fn strip_crlf_consumer_docs() {
        let bot = "---\r\nmodel: p/id\r\ntoken_env: TOK\r\nworkspace: /w\r\n---\r\n\r\n## Методичка\r\nтекст";
        assert_eq!(strip(bot), "## Методичка\r\nтекст");
        let cron = "---\r\ntitle: Тест\r\nschedule: 5m\r\nproject: uwuwu\r\n---\r\n\r\nТело\r\n";
        assert_eq!(strip(cron), "Тело\r\n");
    }
}
