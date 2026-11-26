//! Парс док-спеки бота: fm-шапка (fm_lite, паритет кронов) → BotSpec.
//! Обязательные поля: model (`provider/id[#variant]`), token_env,
//! workspace. Битый fm — громкая ошибка парса (молчаливый пропуск
//! прячет опечатку владельца).

use crate::fm_lite;
use uwuwu_domain::{BotModel, BotSpec};

/// None/error-строки fm считаются незаданными.
fn field<'a>(fm: &'a std::collections::BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    fm.get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Парс док-спеки; Err — человекочитаемая причина (супервизор логирует
/// warn раз на тик — bot.spec_invalid).
pub fn parse_bot_doc(slug: &str, md: &str) -> Result<BotSpec, String> {
    let fm = fm_lite::parse(md);
    let model_raw = field(&fm, "model").ok_or("нет fm model")?;
    let model = parse_model(model_raw)?;
    let token_env = field(&fm, "token_env")
        .ok_or("нет fm token_env")?
        .to_string();
    let workspace = field(&fm, "workspace")
        .ok_or("нет fm workspace")?
        .to_string();
    let name = slug.rsplit('/').next().unwrap_or(slug);
    Ok(BotSpec {
        slug: slug.to_string(),
        title: field(&fm, "title").unwrap_or(name).to_string(),
        enabled: field(&fm, "enabled")
            .map(|v| !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true),
        model,
        token_env,
        owner: field(&fm, "owner").map(|v| v.trim_start_matches('@').to_string()),
        workspace,
        greeting: field(&fm, "greeting").map(String::from),
        agent: field(&fm, "agent").map(String::from),
        manual: fm_lite::strip(md).to_string(),
        // Уроки — соседний док, не fm этого дока: None = «не обогащено»
        // (супервизор дочитывает `bot/<имя>/Уроки` после парса; #1424).
        lessons: None,
    })
}

/// `provider/id[#variant]` → BotModel.
fn parse_model(raw: &str) -> Result<BotModel, String> {
    let (prov_id, variant) = raw
        .split_once('#')
        .map_or((raw, None), |(p, v)| (p, Some(v.trim().to_string())));
    let (provider, id) = prov_id
        .split_once('/')
        .ok_or("model не в форме provider/id")?;
    if provider.trim().is_empty() || id.trim().is_empty() {
        return Err("model: пустой provider или id".into());
    }
    Ok(BotModel {
        provider: provider.trim().to_string(),
        id: id.trim().to_string(),
        variant: variant.filter(|v| !v.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "---\ntitle: Помощник\nmodel: opencode-go/hy4-flash-free#low\n\
token_env: UWUWU_BOT_TOKEN_HELPER\nowner: @yurvon\nworkspace: /home/yurvon/uwuwu-agent\n\
greeting: Привет!\nenabled: false\n---\n\n## Методичка\nсоздавай Draft";

    #[test]
    fn parses_full_doc() {
        let s = parse_bot_doc("bot/помощник", DOC).unwrap();
        assert_eq!(s.title, "Помощник");
        assert_eq!(s.model.provider, "opencode-go");
        assert_eq!(s.model.id, "hy4-flash-free");
        assert_eq!(s.model.variant.as_deref(), Some("low"));
        assert_eq!(s.token_env, "UWUWU_BOT_TOKEN_HELPER");
        assert_eq!(s.owner.as_deref(), Some("yurvon"), "@ срезается");
        assert_eq!(s.workspace, "/home/yurvon/uwuwu-agent");
        assert_eq!(s.greeting.as_deref(), Some("Привет!"));
        assert!(!s.enabled);
        assert!(s.manual.starts_with("## Методичка"));
    }

    #[test]
    fn defaults_and_variantless_model() {
        let md = "---\nmodel: p/id\ntoken_env: TOK\nworkspace: /w\n---\nтело";
        let s = parse_bot_doc("bot/имя", md).unwrap();
        assert_eq!(s.title, "имя", "дефолт тайтла — имя дока");
        assert!(s.enabled, "дефолт enabled");
        assert_eq!(s.model.variant, None);
        assert_eq!(s.owner, None);
        assert_eq!(s.manual, "тело");
    }

    #[test]
    fn enabled_false_is_case_insensitive() {
        // yaml-конвенция False/FALSE — тоже погашено
        for v in ["false", "False", "FALSE"] {
            let md = format!("---\nmodel: p/id\ntoken_env: TOK\nworkspace: /w\nenabled: {v}\n---");
            let s = parse_bot_doc("s", &md).unwrap();
            assert!(!s.enabled, "enabled: {v} должно гасить бота");
        }
    }

    #[test]
    fn missing_or_broken_fields_fail_loudly() {
        assert!(parse_bot_doc("s", "без fm").is_err());
        assert!(
            parse_bot_doc("s", "---\ntitle: t\n---\nтело").is_err(),
            "нет model"
        );
        assert!(
            parse_bot_doc("s", "---\nmodel: без-слэша\nworkspace: /w\n---\n").is_err(),
            "model без provider/id"
        );
        assert!(
            parse_bot_doc("s", "---\nmodel: p/id\n---\n").is_err(),
            "нет workspace"
        );
    }

    /// #1664: LF-шапка и CRLF-тело доходят до парсера; ведущие пустые
    /// строки снимаются, переносы методички остаются.
    #[test]
    fn mixed_doc_manual_has_no_leading_blank_lines() {
        let md =
            "---\nmodel: p/id\ntoken_env: TOK\nworkspace: /w\n---\r\n\r\n## Методичка\r\nтекст";
        let spec = parse_bot_doc("bot/смешанный", md).unwrap();
        assert_eq!(spec.manual, "## Методичка\r\nтекст");
    }

    /// #1664: чистый CRLF-док доходит до `parse`, но LF-строгий парсер
    /// возвращает пустую карту — потребитель отказывает; до `strip` док
    /// не доходит.
    #[test]
    fn crlf_doc_is_rejected_by_lf_strict_parse() {
        let md = "---\r\nmodel: p/id\r\ntoken_env: TOK\r\nworkspace: /w\r\n---\r\n## Методичка";
        assert!(parse_bot_doc("bot/crlf", md).is_err());
    }
}
