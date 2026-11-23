//! Поведение бот-контура как чистые функции: классификация апдейтов
//! (private-only + owner), сборка промпта (шапка + методичка + текст),
//! чанкование реплая, диф-действие супервизора.

use crate::bots::ports::{Incoming, TgMessage};
use crate::fm_lite;
use uwuwu_domain::{BotAction, BotSpec};

/// Классификация входящего: private-only (группы/каналы игнорируются),
/// owner — дополнительный фильтр. Ok — текст или media-факт (#1171).
pub fn classify_message(msg: &TgMessage, owner: Option<&str>) -> Option<Incoming> {
    if msg.chat_type != "private" {
        return None;
    }
    if let Some(expected) = owner {
        let matches = msg
            .from_username
            .as_deref()
            .map(|u| u.eq_ignore_ascii_case(expected))
            .unwrap_or(false);
        if !matches {
            return None;
        }
    }
    let text = msg
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from);
    match text {
        Some(t) => Some(Incoming::Text(t)),
        None => msg.media.clone().map(|media| Incoming::Media {
            caption: msg
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from),
            media,
        }),
    }
}

/// Промпт сессии бота: методичка — префикс каждого сообщения (горячая
/// правка доезжает за тик супервизора; шапка подавляет устаревшие
/// инструкции в истории сессии).
pub fn build_prompt(manual: &str, text: &str) -> String {
    format!(
        "The bot manual is the only valid version; earlier instructions \
         in the session history are stale.\n\n## Manual\n\n{manual}\n\n## User message\n\n{text}"
    )
}

/// Композит методички для дозаправки (#1424): методичка-основа владельца
/// плюс блок уроков бота. Пустые уроки — методичка как прежде
/// (регресс-гейт: дока `Уроки` нет — текст дозаправки не меняется).
pub fn compose_manual(manual: &str, lessons: &str) -> String {
    if lessons.trim().is_empty() {
        return manual.to_string();
    }
    format!(
        "{manual}\n\n## Уроки (выучено автоматически; пишет ночной крон)\n\n{}",
        lessons.trim()
    )
}

/// Промпт доставки уроков (agent-режим, #1424): уроки едут одним
/// сообщением при изменении (hash-гейт в state), дальше чистый текст;
/// шапка подавляет устаревшие копии уроков в истории сессии.
pub fn build_lessons_prompt(lessons: &str, text: &str) -> String {
    format!(
        "New bot lessons (auto-learned rules) below; earlier lesson copies \
         in the session history are stale.\n\n## Lessons\n\n{lessons}\n\n## User message\n\n{text}"
    )
}

/// Промпт отзыва уроков (agent-режим, #1424): носитель опустел
/// (компактизация/поглощение методичкой/отзыв) — ранние копии уроков
/// в истории сессии устарели и исполнению не подлежат.
pub fn build_lessons_revoked_prompt(text: &str) -> String {
    format!(
        "The bot lessons carrier is now empty: earlier lesson copies in the \
         session history are stale and must not be followed.\n\n## User message\n\n{text}"
    )
}

/// Нормализация носителя уроков (#1424): fm-шапка (если появилась) —
/// долой, обрамление — трим. Конвенция — body-only док; strip защитен.
pub fn normalize_lessons(raw: &str) -> String {
    fm_lite::strip(raw).trim().to_string()
}

/// Чанки ≤4000 символов (лимит ТГ 4096, запас); разрыв — по границе
/// символов, предпочитая перевод строки в хвосте чанка.
pub fn chunk_reply(text: &str) -> Vec<String> {
    const LIMIT: usize = 4000;
    let mut out = Vec::new();
    let mut rest = text;
    while rest.chars().count() > LIMIT {
        let cut = rest
            .char_indices()
            .nth(LIMIT)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let head = &rest[..cut];
        let split = head
            .rfind('\n')
            .filter(|i| cut - i > LIMIT / 4)
            .map_or(cut, |i| i + 1);
        out.push(head[..split].trim_end().to_string());
        rest = &rest[split..];
    }
    if !out.is_empty() || !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Диф запущенного/желаемого: действие супервизора (None — менять нечего,
/// метаданные/методичка обновляются живьём мимо дифа).
pub fn plan_action(running: &BotSpec, desired: &BotSpec) -> Option<BotAction> {
    if !desired.enabled {
        return Some(BotAction::Stop);
    }
    if desired.structural_changed(running) {
        return Some(BotAction::RecreateSession);
    }
    if desired.token_changed(running) {
        return Some(BotAction::ReArm);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::ports::MediaKind;

    fn msg(chat_type: &str, username: Option<&str>, text: Option<&str>) -> TgMessage {
        TgMessage {
            chat_type: chat_type.into(),
            chat_id: 42,
            from_username: username.map(String::from),
            text: text.map(String::from),
            caption: None,
            media: None,
            forward: None,
        }
    }

    fn photo(caption: Option<&str>) -> TgMessage {
        TgMessage {
            chat_type: "private".into(),
            chat_id: 42,
            from_username: Some("yurvon".into()),
            text: None,
            caption: caption.map(String::from),
            media: Some(crate::bots::ports::TgMedia {
                kind: MediaKind::Photo,
                file_id: "agg-file".into(),
                file_name: None,
                file_size: Some(10_000),
                mime: Some("image/jpeg".into()),
            }),
            forward: None,
        }
    }

    #[test]
    fn private_only_and_owner_filter() {
        assert_eq!(
            classify_message(&msg("private", Some("yurvon"), Some("привет")), None),
            Some(Incoming::Text("привет".into()))
        );
        assert_eq!(
            classify_message(&msg("group", Some("yurvon"), Some("привет")), None),
            None
        );
        assert_eq!(
            classify_message(&msg("channel", None, Some("x")), None),
            None
        );
        // owner задан: чужие и безюзерные не проходят
        assert_eq!(
            classify_message(&msg("private", Some("stranger"), Some("x")), Some("yurvon")),
            None
        );
        assert_eq!(
            classify_message(&msg("private", None, Some("x")), Some("yurvon")),
            None
        );
        // кейс username нечувствителен
        assert_eq!(
            classify_message(&msg("private", Some("Yurvon"), Some("x")), Some("yurvon")),
            Some(Incoming::Text("x".into()))
        );
        // пустой текст не проходит
        assert_eq!(
            classify_message(&msg("private", Some("yurvon"), Some("  ")), None),
            None
        );
    }

    /// #1171: медиа от владельца — не тихий skip, а Incoming::Media
    /// (caption как подпись; без caption — None).
    #[test]
    fn owner_media_classified_even_without_text() {
        let got = classify_message(&photo(Some("от МПНЧ")), Some("yurvon"));
        assert_eq!(
            got,
            Some(Incoming::Media {
                caption: Some("от МПНЧ".into()),
                media: crate::bots::ports::TgMedia {
                    kind: MediaKind::Photo,
                    file_id: "agg-file".into(),
                    file_name: None,
                    file_size: Some(10_000),
                    mime: Some("image/jpeg".into()),
                },
            })
        );
        // чужое медиа — прежний owner-фильтр
        let mut stranger = photo(None);
        stranger.from_username = Some("stranger".into());
        assert_eq!(classify_message(&stranger, Some("yurvon")), None);
        // медиа в группе — молча
        let mut group = photo(None);
        group.chat_type = "group".into();
        assert_eq!(classify_message(&group, None), None);
    }

    #[test]
    fn prompt_carries_manual_prefix() {
        let p = build_prompt("создавай Draft", "заявка");
        assert!(p.starts_with("The bot manual is the only valid version"));
        assert!(p.contains("создавай Draft"));
        assert!(p.ends_with("заявка"));
    }

    /// #1424: пустые уроки — композит тождественен методичке (регресс-гейт),
    /// непустые — блок уроков после методички.
    #[test]
    fn compose_manual_appends_lessons_block() {
        assert_eq!(compose_manual("методичка", ""), "методичка");
        assert_eq!(compose_manual("методичка", "   "), "методичка");
        let composed = compose_manual("методичка", "урок один");
        assert!(composed.starts_with("методичка\n\n## Уроки"));
        assert!(composed.ends_with("урок один"));
    }

    /// #1424: промпт уроков несёт шапку-подавитель устаревших копий,
    /// уроки и текст пользователя.
    #[test]
    fn lessons_prompt_carries_prefix() {
        let p = build_lessons_prompt("урок один", "вопрос");
        assert!(p.starts_with("New bot lessons"));
        assert!(p.contains("урок один"));
        assert!(p.ends_with("вопрос"));
    }

    /// #1424 (ревью Common 1): промпт отзыва — носитель опустел, ранние
    /// копии уроков в истории объявлены устаревшими.
    #[test]
    fn lessons_revoked_prompt_suppresses_history() {
        let p = build_lessons_revoked_prompt("вопрос");
        assert!(p.starts_with("The bot lessons carrier is now empty"));
        assert!(p.ends_with("вопрос"));
    }

    /// #1424: нормализация носителя уроков — fm-шапка срезается,
    /// body-only док проходит как есть, обрамление триммится.
    #[test]
    fn normalize_lessons_strips_fm_and_trims() {
        assert_eq!(
            normalize_lessons("---\ntitle: Уроки\n---\n\nурок один"),
            "урок один"
        );
        assert_eq!(normalize_lessons("  урок один\n"), "урок один");
        assert_eq!(normalize_lessons(""), "");
    }

    #[test]
    fn chunks_at_limit_preferring_newline() {
        assert_eq!(chunk_reply("короткий"), vec!["короткий"]);
        assert!(chunk_reply("").is_empty());
        let long = "а".repeat(9500);
        let chunks = chunk_reply(&long);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4000));
        assert_eq!(chunks.concat().chars().count(), 9500);
        // разрыв предпочитает перевод строки: он глубже последней четверти
        // головы (cut − i > 1000) — ломаем по нему, а не посимвольно
        let text = format!("{}\n{}хвост", "а".repeat(2999), "б".repeat(1000));
        let chunks = chunk_reply(&text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4000));
        assert_eq!(chunks[0], "а".repeat(2999), "хвостовой \\n срезан");
        assert_eq!(chunks[1], format!("{}хвост", "б".repeat(1000)));
    }

    #[test]
    fn diff_actions_by_tier() {
        let running = fixture();
        let same = fixture();
        assert_eq!(
            plan_action(&running, &same),
            None,
            "без изменений — hot-путь"
        );
        let disabled = BotSpec {
            agent: None,
            enabled: false,
            ..fixture()
        };
        assert_eq!(plan_action(&running, &disabled), Some(BotAction::Stop));

        let rearm = BotSpec {
            agent: None,
            token_env: "OTHER".into(),
            ..fixture()
        };
        assert_eq!(plan_action(&running, &rearm), Some(BotAction::ReArm));
        let recreate = BotSpec {
            workspace: "/w2".into(),
            ..fixture()
        };
        assert_eq!(
            plan_action(&running, &recreate),
            Some(BotAction::RecreateSession)
        );
        let moved = BotSpec {
            slug: "other".into(),
            ..fixture()
        };
        assert_eq!(plan_action(&running, &moved), None, "slug — не поле дифа");
    }

    fn fixture() -> BotSpec {
        BotSpec {
            agent: None,
            slug: "bot/x".into(),
            title: "x".into(),
            enabled: true,
            model: uwuwu_domain::BotModel {
                provider: "p".into(),
                id: "a".into(),
                variant: None,
            },
            token_env: "TOKEN".into(),
            owner: None,
            workspace: "/w".into(),
            greeting: None,
            manual: "м".into(),
            lessons: None,
        }
    }
}
