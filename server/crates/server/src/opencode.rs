//! opencode-исполнитель: REST-клиент, реализация AgentSessionGateway.
//! Endpoints сверен с openapi опенкода 2.3.0.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::{AgentSessionGateway, Liveness, SessionSpec};

pub struct OpencodeClient {
    base: String,
    password: String,
    http: reqwest::Client,
}

/// HTTP-статус → liveness-вердикт (маппинг вынесен для тестов без сети).
pub fn liveness_of_status(status: reqwest::StatusCode) -> Liveness {
    if status.is_success() {
        Liveness::Alive
    } else if status.as_u16() == 404 || status.as_u16() == 400 {
        Liveness::Dead
    } else {
        Liveness::Unknown
    }
}

impl OpencodeClient {
    pub fn new(base: String, password: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn call(
        &self,
        path: &str,
        method: reqwest::Method,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .basic_auth("opencode", Some(&self.password));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Session(format!("transport {path}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Session(format!("body {path}: {e}")))?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    fn check(status: reqwest::StatusCode, what: &str) -> UsecaseResult<()> {
        if status.is_success() {
            Ok(())
        } else {
            Err(UsecaseError::Session(format!("{what}: HTTP {status}")))
        }
    }
}

#[async_trait]
impl AgentSessionGateway for OpencodeClient {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let mut body = json!({
            "title": spec.title.chars().take(64).collect::<String>(),
            "location": { "directory": spec.directory },
        });
        if let Some(agent) = &spec.agent {
            body["agent"] = json!(agent);
        }
        if let Some((provider, id)) = &spec.model {
            body["model"] = json!({ "providerID": provider, "id": id });
        }
        let (status, resp) = self
            .call("/session", reqwest::Method::POST, Some(body))
            .await?;
        Self::check(status, "create_session")?;
        let id = resp["data"]["id"]
            .as_str()
            .or(resp["id"].as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return Err(UsecaseError::Session("create_session: пустой id".into()));
        }
        Ok(id)
    }

    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                &format!("/session/{session}/prompt"),
                reqwest::Method::POST,
                Some(json!({ "text": text })),
            )
            .await?;
        Self::check(status, &format!("prompt {session}"))
    }

    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                &format!("/session/{session}/command"),
                reqwest::Method::POST,
                Some(json!({ "name": name, "text": args })),
            )
            .await?;
        Self::check(status, &format!("command {session}/{name}"))
    }

    async fn liveness(&self, session: &str) -> Liveness {
        match self
            .call(&format!("/session/{session}"), reqwest::Method::GET, None)
            .await
        {
            Ok((status, _)) => liveness_of_status(status),
            Err(_) => Liveness::Unknown,
        }
    }

    async fn delete_session(&self, session: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                &format!("/session/{session}"),
                reqwest::Method::DELETE,
                None,
            )
            .await?;
        // 404 = уже нет — идемпотентно ок
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(UsecaseError::Session(format!(
                "delete {session}: HTTP {status}"
            )))
        }
    }

    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>> {
        let (status, resp) = self
            .call(
                &format!("/session/{session}/message?limit=1&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        Ok(resp
            .as_array()
            .and_then(|msgs| msgs.first())
            .and_then(|m| m["time"]["updated"].as_u64()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_maps_statuses() {
        assert_eq!(liveness_of_status(reqwest::StatusCode::OK), Liveness::Alive);
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::NOT_FOUND),
            Liveness::Dead
        );
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::BAD_REQUEST),
            Liveness::Dead
        );
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::BAD_GATEWAY),
            Liveness::Unknown
        );
    }

    #[test]
    fn base_trims_trailing_slash() {
        let c = OpencodeClient::new("https://x.y/".into(), "p".into());
        assert_eq!(c.base, "https://x.y");
    }
}
