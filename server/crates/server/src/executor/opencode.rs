//! opencode-исполнитель: REST-клиент, реализация AgentSessionGateway.
//! Endpoints сверен с openapi опенкода 2.3.0.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_usecase::{AgentSessionGateway, AssistantReply, Liveness, SessionInfo, SessionSpec};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

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
    /// gate_cookie — значение Cookie-хедера целиком (oc_gate=...):
    /// потребляется в дефолтные хедеры клиента, полем не хранится.
    pub fn new(base: String, password: String, gate_cookie: Option<String>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(cookie) = &gate_cookie {
            let value = reqwest::header::HeaderValue::from_str(cookie).expect("gate cookie: ascii");
            headers.insert(reqwest::header::COOKIE, value);
        }
        Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            http: reqwest::Client::builder()
                .default_headers(headers)
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

/// Тело POST /session из спека. Контракт моделей (#1154): модель —
/// карта сервера, кладётся в тело всегда (body.model перекрывает деф
/// агента — деф-файлы моделей не несут; решение владельца 17.11).
fn create_session_body(spec: &SessionSpec) -> Value {
    let mut body = json!({
        "title": spec.title.chars().take(64).collect::<String>(),
        "location": { "directory": spec.directory },
    });
    if let Some(name) = &spec.custom_agent {
        // именованный агент опенкода (agents/<name>.md в cwd):
        // боты с fm agent — методичка вшита системой (#349)
        body["agent"] = json!(name);
    } else {
        match &spec.agent {
            uwuwu_domain::AgentSlot::Role(agent) => {
                body["agent"] = json!(uwuwu_usecase::agent_name(*agent));
            }
            uwuwu_domain::AgentSlot::Default => {}
        }
    }
    let mut m = json!({ "providerID": spec.model.provider(), "id": spec.model.id() });
    if let Some(v) = &spec.variant {
        m["variant"] = json!(v);
    }
    body["model"] = m;
    body
}

#[async_trait]
impl AgentSessionGateway for OpencodeClient {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let body = create_session_body(spec);
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

    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        let (status, resp) = self
            .call("/session?limit=100", reqwest::Method::GET, None)
            .await?;
        Self::check(status, "list sessions")?;
        let raw = if resp.is_array() {
            resp
        } else {
            resp["data"].clone()
        };
        Ok(raw
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|s| {
                        Some(SessionInfo {
                            id: s["id"].as_str()?.to_string(),
                            agent: s["agent"].as_str().unwrap_or_default().to_string(),
                            updated_ms: s["time"]["updated"].as_u64().unwrap_or_default(),
                            outcome: s["outcome"].as_str().unwrap_or_default().to_string(),
                            total_tokens: s["tokens"]["input"].as_u64().unwrap_or_default()
                                + s["tokens"]["output"].as_u64().unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn export_session(&self, session: &str) -> UsecaseResult<String> {
        let (status, resp) = self
            .call(
                &format!("/experimental/session/{session}/export"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("export {session}"))?;
        // экспорт может быть data-обёрнут или plain-текстом
        if let Some(text) = resp["data"].as_str() {
            return Ok(text.to_string());
        }
        if let Some(text) = resp.as_str() {
            return Ok(text.to_string());
        }
        Ok(resp.to_string())
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

    /// Форма хвоста сверена живой пробой опенкода 2.3.0 (задача #1150):
    /// сообщения несут `type` («user»|«assistant»|…), у ассистента —
    /// `content[].text`, `finish` и `time.created`; лимит 5 — в хвосте
    /// могут висеть служебные события (idle) поверх ответа.
    async fn last_assistant_reply(&self, session: &str) -> UsecaseResult<Option<AssistantReply>> {
        let (status, resp) = self
            .call(
                &format!("/session/{session}/message?limit=5&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        let raw = if resp.is_array() {
            resp
        } else {
            resp["data"].clone()
        };
        let Some(msg) = raw
            .as_array()
            .and_then(|msgs| msgs.iter().find(|m| m["type"] == "assistant"))
        else {
            return Ok(None);
        };
        let text: Vec<&str> = msg["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(AssistantReply {
            created_ms: msg["time"]["created"].as_u64().unwrap_or_default(),
            text: text.join("\n"),
            finished: msg["finish"].as_str().is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwuwu_domain::{Agent, AgentSlot, ModelRef};

    fn spec(agent: AgentSlot, model: ModelRef, variant: Option<&str>) -> SessionSpec {
        SessionSpec {
            directory: "/tmp".into(),
            title: "t".into(),
            agent,
            custom_agent: None,
            model,
            variant: variant.map(str::to_string),
            bootstrap: false,
        }
    }

    #[test]
    fn body_always_carries_model_from_server_map() {
        // #1154 (решение владельца 17.11): модель — карта сервера, тело
        // спавна несёт её всегда (деф-файлы моделей не несут)
        let with = create_session_body(&spec(
            AgentSlot::Role(Agent::Dreamer),
            ModelRef::new("zai-coding-plan", "glm-5.3-flash"),
            Some("high"),
        ));
        assert_eq!(with["agent"], "dreamer");
        assert_eq!(with["model"]["providerID"], "zai-coding-plan");
        assert_eq!(with["model"]["id"], "glm-5.3-flash");
        assert_eq!(with["model"]["variant"], "high");

        let without = create_session_body(&spec(
            AgentSlot::Default,
            ModelRef::new("zai-coding-plan", "glm-5.3"),
            None,
        ));
        assert!(without.get("agent").is_none(), "Default — без agent");
        assert_eq!(
            without["model"]["id"], "glm-5.3",
            "модель в теле всегда (карта сервера)"
        );
    }

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
        let c = OpencodeClient::new("https://x.y/".into(), "p".into(), None);
        assert_eq!(c.base, "https://x.y");
    }

    #[test]
    fn gate_cookie_accepted() {
        // кука с ascii-значением проходит в хедеры без паники
        let c = OpencodeClient::new(
            "https://x.y".into(),
            "p".into(),
            Some("oc_gate=g77a99dcff9257cb30992ef970d7e8d1668d1130f".into()),
        );
        assert_eq!(c.base, "https://x.y");
    }
}
