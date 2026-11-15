//! Нативные поля issue (fm-зеркало снесено решением владельца 15.11):
//! сессии/ветка/PR/тип/приоритет живут в глобальных полях issue.
//! Контракт: каталог GET /user/issue-fields (id по имени), чтение —
//! issue.issue_field_values (inline в GET issue), запись — батч POST,
//! очистка — DELETE (404 = no-op). Живые пробы: старый cli (#1092).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use uwuwu_usecase::IssueFieldStore;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub const F_DEV_SESSION: &str = "Сессия dev";
pub const F_REVIEW_SESSION: &str = "Сессия ревью";
pub const F_TEST_SESSION: &str = "Сессия тест";

/// Карта значений полей задачи (непустые).
pub type Fields = HashMap<String, String>;

pub struct IssueFields {
    repo: String,
    token: String,
    http: reqwest::Client,
    catalog: Mutex<Option<HashMap<String, u64>>>,
}

impl IssueFields {
    pub fn new(repo: String, token: String) -> Self {
        Self {
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            catalog: Mutex::new(None),
        }
    }

    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("fields transport {path}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("fields body {path}: {e}")))?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Каталог полей (id по имени), кэш на процесс.
    async fn catalog(&self) -> UsecaseResult<HashMap<String, u64>> {
        {
            let guard = self.catalog.lock().expect("catalog lock");
            if let Some(c) = guard.as_ref() {
                return Ok(c.clone());
            }
        }
        let (status, body) = self
            .rest(reqwest::Method::GET, "/user/issue-fields", None)
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!("каталог полей: HTTP {status}")));
        }
        let mut map = HashMap::new();
        if let Some(list) = body.as_array() {
            for f in list {
                if let (Some(id), Some(name)) = (f["id"].as_u64(), f["name"].as_str()) {
                    map.insert(name.to_string(), id);
                }
            }
        }
        *self.catalog.lock().expect("catalog lock") = Some(map.clone());
        Ok(map)
    }

    /// Чтение полей задачи из GET issue (issue_field_values inline).
    pub async fn load(&self, number: u64) -> UsecaseResult<Fields> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "fields load #{number}: HTTP {status}"
            )));
        }
        Ok(parse_field_values(&body))
    }

    /// Батч-запись: Some — значение, None — очистка (идемпотентно).
    pub async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        let catalog = self.catalog().await?;
        let mut batch = Vec::new();
        for (name, value) in patch {
            let fid = catalog.get(*name).ok_or_else(|| {
                UsecaseError::Board(format!("глобальное поле «{name}» не найдено"))
            })?;
            match value {
                Some(v) if !v.is_empty() => batch.push(json!({ "field_id": fid, "value": v })),
                _ => {
                    // очистка: 404 (значения не было) — no-op
                    let (st, _) = self
                        .rest(
                            reqwest::Method::DELETE,
                            &format!(
                                "/repos/{}/issues/{number}/issue-field-values/{fid}",
                                self.repo
                            ),
                            None,
                        )
                        .await?;
                    if !st.is_success() && st.as_u16() != 404 {
                        return Err(UsecaseError::Board(format!(
                            "fields clear #{number}:{name}: HTTP {st}"
                        )));
                    }
                }
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        let (status, _) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues/{number}/issue-field-values", self.repo),
                Some(json!({ "issue_field_values": batch })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "fields store #{number}: HTTP {status}"
            )));
        }
        Ok(())
    }

    /// Создать глобальное поле, если его нет (каталог дрогнуть).
    #[allow(dead_code)] // применяется при заведении «Тип» первым create
    pub async fn ensure_field(&self, name: &str) -> UsecaseResult<()> {
        let catalog = self.catalog().await?;
        if catalog.contains_key(name) {
            return Ok(());
        }
        let (status, _resp) = self
            .rest(
                reqwest::Method::POST,
                "/user/issue-fields",
                Some(json!({ "name": name, "type": "text" })),
            )
            .await?;
        if !status.is_success() && status.as_u16() != 422 {
            return Err(UsecaseError::Board(format!(
                "создание поля «{name}»: HTTP {status}"
            )));
        }
        *self.catalog.lock().expect("catalog lock") = None;
        Ok(())
    }
}

/// issue_field_values → карта (name → value; select — имя опции).
pub fn parse_field_values(issue: &Value) -> Fields {
    let mut out = Fields::new();
    if let Some(list) = issue["issue_field_values"].as_array() {
        for f in list {
            let name = f["issue_field_name"].as_str().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let value = f["single_select_option"]["name"]
                .as_str()
                .or(f["value"].as_str())
                .unwrap_or_default();
            if !value.is_empty() {
                out.insert(name.to_string(), value.to_string());
            }
        }
    }
    out
}

#[async_trait]
impl IssueFieldStore for IssueFields {
    async fn load(&self, number: u64) -> UsecaseResult<Fields> {
        IssueFields::load(self, number).await
    }

    async fn store(&self, number: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        IssueFields::store(self, number, patch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_values() {
        let issue = json!({
            "issue_field_values": [
                { "issue_field_name": "Branch", "value": "work/task-9" },
                { "issue_field_name": "Сессия dev", "value": "ses_a" },
                { "issue_field_name": "Приоритет", "single_select_option": { "name": "high" } },
                { "issue_field_name": "PR", "value": "" },
            ]
        });
        let f = parse_field_values(&issue);
        assert_eq!(f.get("Branch").map(String::as_str), Some("work/task-9"));
        assert_eq!(f.get("Сессия dev").map(String::as_str), Some("ses_a"));
        assert_eq!(f.get("Приоритет").map(String::as_str), Some("high"));
        assert!(!f.contains_key("PR"), "пустые значения не в карте");
    }
}
