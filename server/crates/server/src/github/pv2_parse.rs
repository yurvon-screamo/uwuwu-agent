//! Парсеры Pv2 REST-ответов (чистые функции, юнит-пины на живых формах
//! пробы среза 0 — тело #1236). Формы: field/item id — numeric, id опции —
//! hex-строка; Status single-select живёт в `value.name.raw`;
//! пагинация list items — Link-курсор `after`, параметр `page` игнорируется.

use serde_json::Value;
use std::collections::HashMap;
use uwuwu_usecase::UsecaseError;

use super::pv2::Pv2Meta;

/// Каталог Status-поля из GET /projectsV2/{n}/fields: проект по тайтлу,
/// numeric field id, карта «имя опции → hex-идентификатор».
pub(crate) fn parse_field_catalog(
    project_number: u64,
    fields: &Value,
    board: &str,
) -> Result<Pv2Meta, UsecaseError> {
    let field = fields
        .as_array()
        .and_then(|list| list.iter().find(|f| f["name"] == "Status"))
        .ok_or_else(|| {
            UsecaseError::Board(format!(
                "доска «{board}»: Status-поле в каталоге не найдено"
            ))
        })?;
    let status_field_id = field["id"]
        .as_u64()
        .ok_or_else(|| UsecaseError::Board(format!("доска «{board}»: Status-поле без id")))?;
    let mut options = HashMap::new();
    if let Some(list) = field["options"].as_array() {
        for option in list {
            let name = option["name"]["raw"].as_str().unwrap_or_default();
            let id = option["id"].as_str().unwrap_or_default();
            if !name.is_empty() && !id.is_empty() {
                options.insert(name.to_string(), id.to_string());
            }
        }
    }
    Ok(Pv2Meta {
        project_number,
        status_field_id,
        options,
    })
}

/// Raw-имя Status из fields-массива item-ответа: Some(raw) — значение;
/// None — Status-поля в ответе нет (не задан или read-лаг Projects v2 —
/// проба среза 0: GET сразу после PATCH отдал fields без Status).
pub(crate) fn parse_status_value(fields: &Value) -> Option<String> {
    let list = fields.as_array()?;
    let field = list.iter().find(|f| f["name"] == "Status")?;
    Some(
        field["value"]["name"]["raw"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// `after`-курсор из Link-заголовка list items: `rel="next"` ветка,
/// значение параметра after (проба: page= игнорируется, только курсор).
pub(crate) fn next_after(link: Option<&str>) -> Option<String> {
    let link = link?;
    for branch in link.split(',') {
        let Some((url_part, rel_part)) = branch.split_once(';') else {
            continue;
        };
        let rel = rel_part.trim().trim_start_matches("rel=").trim_matches('"');
        if rel != "next" {
            continue;
        }
        let after = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .split('&')
            .find_map(|param| param.strip_prefix("after="));
        if let Some(after) = after {
            return Some(after.to_string());
        }
    }
    None
}

/// Резолв numeric item id строки из элемента list items (number, id).
pub(crate) fn parse_item_entry(item: &Value) -> Option<(u64, u64)> {
    Some((item["content"]["number"].as_u64()?, item["id"].as_u64()?))
}
