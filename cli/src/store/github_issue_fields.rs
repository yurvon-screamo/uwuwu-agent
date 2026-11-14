//! Глобальные issue fields — носитель клеток задачи (ADR-098: мета не в md).
//!
//! Контракт живых проб (issue #1092, задача 1087):
//! - батч-POST `POST repos/{repo}/issues/{n}/issue-field-values` идемпотентен
//!   (overwrite); тело `{"issue_field_values":[{"field_id":N,"value":V}]}`;
//! - text/date — значением-строкой, single_select — **именем опции строкой**
//!   («on», «high»; числовой id даёт 422 «must be a string option name»);
//! - очистка — `DELETE repos/{repo}/issues/{n}/issue-field-values/{field_id}`,
//!   идемпотентен: повтор на отсутствующем значении — exit 0, пустой вывод
//!   (проба-2 14.11; «404» из пробы-1 — артефакт -i-режима gh); POST с
//!   пустой строкой — HTTP 500 (очистка только DELETE);
//! - чтение: `issue_field_values` прямо в payload GET issues (и list тоже):
//!   text/date — `value`, select — `single_select_option.name`; отсутствующее
//!   поле = пустая клетка.
//!
//! Имена полей — канон владельца (`/user/issue-fields`, созданы 13–14.11).

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Имена глобальных полей — канон носителя (ADR-098).
pub(crate) mod names {
    pub const PRIORITY: &str = "Приоритет";
    pub const AUTODONE: &str = "Autodone";
    pub const BRANCH: &str = "Branch";
    pub const PR: &str = "PR";
    pub const DEV_SESSION: &str = "Сессия dev";
    pub const SCHEDULE: &str = "Расписание";
    pub const PLANNED: &str = "Запланировано";
}
// Поля «Сессия ревью»/«Сессия тест» живут на носителе, но cli их не читает
// и не пишет (ячейки фабрики — TS-шов update_database_row); константы
// в Rust не заводим — dead code.

/// Значение клетки для записи (норм-форма носителя).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FieldValue {
    /// Branch/PR/Сессии/Расписание — строка.
    Text(String),
    /// Single_select: имя опции строкой («high», «on»).
    Select(String),
    /// Date: YYYY-MM-DD.
    Date(String),
}
/// Норм-форма клеток из payload issue: `issue_field_values` →
/// {имя поля: значение}. select — имя опции; text/date — value;
/// отсутствующее поле в карте нет (пустая клетка).
pub(crate) fn fields_of(issue: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for f in issue["issue_field_values"].as_array().unwrap_or(&vec![]) {
        let name = f["issue_field_name"].as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let value = f["single_select_option"]["name"]
            .as_str()
            .or_else(|| f["value"].as_str())
            .unwrap_or("");
        if !value.is_empty() {
            out.insert(name.to_string(), value.to_string());
        }
    }
    out
}

/// field_id по имени из GET /user/issue-fields. Без кэша: на каждую клетку
/// один REST (set_fields зовёт per-cell) — мутация N клеток = N запросов
/// каталога; для батчей бэкфилла это заметный хвост (183 строки × клетки),
/// в rate-бюджете учтён. Кэширование — при первом упоре.
pub(crate) async fn field_id(name: &str) -> Result<i64> {
    let fields = super::github_gh::gh_json(&["api", "/user/issue-fields"], None).await?;
    fields
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|f| f.get("name").and_then(|n| n.as_str()) == Some(name))
                .and_then(|f| f.get("id").and_then(|i| i.as_i64()))
        })
        .with_context(|| format!("глобальное поле «{name}» не найдено"))
}

/// Батч-запись клеток. `None` — очистка клетки (DELETE; 404 — no-op).
/// Не-None собираются в один POST (идемпотентный overwrite).
pub(crate) async fn set_fields(
    issue_number: &str,
    repo: &str,
    fields: &[(&str, Option<FieldValue>)],
) -> Result<()> {
    let mut batch: Vec<Value> = Vec::new();
    for (name, value) in fields {
        let fid = field_id(name).await?;
        match value {
            None => {
                // Очистка: DELETE — идемпотентен (повтор на отсутствующем
                // значении: exit 0, пустой вывод; живая проба-2 #1092)
                let args = [
                    "api",
                    "-X",
                    "DELETE",
                    &format!("repos/{repo}/issues/{issue_number}/issue-field-values/{fid}"),
                ];
                super::github_gh::gh_json(&args, None)
                    .await
                    .with_context(|| format!("поле «{name}» не очищено"))?;
            }
            Some(FieldValue::Text(s)) => {
                batch.push(serde_json::json!({ "field_id": fid, "value": s }))
            }
            Some(FieldValue::Select(s)) => {
                batch.push(serde_json::json!({ "field_id": fid, "value": s }))
            }
            Some(FieldValue::Date(d)) => {
                batch.push(serde_json::json!({ "field_id": fid, "value": d }))
            }
        }
    }
    if batch.is_empty() {
        return Ok(());
    }
    super::github_gh::gh_json(
        &[
            "api",
            "-X",
            "POST",
            &format!("repos/{repo}/issues/{issue_number}/issue-field-values"),
        ],
        Some(&serde_json::json!({ "issue_field_values": batch }).to_string()),
    )
    .await
    .with_context(|| format!("клетки issue #{issue_number} не записаны"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_of_разбирает_норм_форму() {
        let issue = serde_json::json!({
            "issue_field_values": [
                { "issue_field_name": "Приоритет", "value": 87264165,
                  "single_select_option": { "id": 87264165, "name": "high" } },
                { "issue_field_name": "Branch", "value": "work/x" },
                { "issue_field_name": "Запланировано", "value": "2026-11-20" },
                { "issue_field_name": "Autodone", "value": 87264196,
                  "single_select_option": { "id": 87264196, "name": "on" } }
            ]
        });
        let f = fields_of(&issue);
        assert_eq!(f.get("Приоритет").map(String::as_str), Some("high"));
        assert_eq!(f.get("Branch").map(String::as_str), Some("work/x"));
        assert_eq!(
            f.get("Запланировано").map(String::as_str),
            Some("2026-11-20")
        );
        assert_eq!(f.get("Autodone").map(String::as_str), Some("on"));
    }

    #[test]
    fn fields_of_пустой_payload_пустая_карта() {
        let issue = serde_json::json!({ "issue_field_values": [] });
        assert!(fields_of(&issue).is_empty());
        // поле в payload нет вовсе (issue_field_values отсутствует) — пусто
        let issue = serde_json::json!({ "number": 1 });
        assert!(fields_of(&issue).is_empty());
    }

    #[test]
    fn fields_of_пустое_значение_не_попадает() {
        // text-поле со значением "" в payload не встречается (очистка = DELETE
        // и поле исчезает), но защитимся: пустое значение — не клетка
        let issue = serde_json::json!({
            "issue_field_values": [ { "issue_field_name": "Branch", "value": "" } ]
        });
        assert!(fields_of(&issue).is_empty());
    }
}
