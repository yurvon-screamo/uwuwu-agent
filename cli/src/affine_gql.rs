//! Direct AFFiNE GraphQL access for comment replies. The MCP gateway has no
//! reply tool (upstream affine-mcp-server v3.11.0 exposes only list/create/
//! update/delete/resolve), so thread replies go straight to the AFFiNE UI API
//! with the bot's UI credentials — the same sign-in pattern as ServerSearch.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

use crate::affine_search_server::ServerSearchConfig;
use crate::affine_snapshot;

/// Transport failure is retried once with this pause — the UI API shares the
/// tunnel with the gateway, one transient reset is normal under load.
const TRANSPORT_RETRY_PAUSE: Duration = Duration::from_secs(1);

pub struct AffineUi {
    http: reqwest::Client,
    ui_url: String,
}

impl AffineUi {
    /// One session: sign in with email/password, keep the cookie jar.
    pub async fn connect(config: &ServerSearchConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("http client for affine ui")?;
        let resp = http
            .post(format!("{}/api/auth/sign-in", config.ui_url))
            .json(&json!({"email": config.email, "password": config.password}))
            .send()
            .await
            .context("affine sign-in request")?;
        if !resp.status().is_success() {
            anyhow::bail!("affine sign-in failed: HTTP {}", resp.status());
        }
        Ok(Self {
            http,
            ui_url: config.ui_url.clone(),
        })
    }

    /// Reply into an existing thread: comment_id is the top-level comment id
    /// (see CommentThread). Returns the created reply id.
    pub async fn create_reply(
        &self,
        comment_id: &str,
        text: &str,
        doc_title: &str,
    ) -> Result<String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let meta_id = affine_snapshot::snapshot_meta_id(comment_id, now_ms);
        let body = json!({
            "query": "mutation CreateReply($input: ReplyCreateInput!) { createReply(input: $input) { id commentId } }",
            "variables": {
                "input": {
                    "commentId": comment_id,
                    "content": affine_snapshot::text_to_snapshot(text, &meta_id, now_ms),
                    "docMode": "page",
                    "docTitle": doc_title,
                }
            }
        });
        let payload = self.query(&body).await?;
        payload
            .pointer("/data/createReply/id")
            .and_then(|id| id.as_str())
            .map(String::from)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "createReply: ответ без id: {}",
                    cut(&payload.to_string(), 200)
                )
            })
    }

    /// POST a full request body (query + variables), one retry on transport
    /// failures and HTTP 5xx. Returns the raw payload — callers check
    /// `errors` themselves (partial data with errors is possible in GraphQL).
    pub(crate) async fn query(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/graphql", self.ui_url);
        let send = || async { self.http.post(&url).json(body).send().await };
        let resp = match send().await {
            Ok(resp) if resp.status().is_server_error() => {
                let status = resp.status();
                tokio::time::sleep(TRANSPORT_RETRY_PAUSE).await;
                let retried = send().await.map_err(|second| {
                    anyhow::anyhow!("affine graphql: HTTP {status}; retry: {second}")
                })?;
                retried.error_for_status()?
            }
            Ok(resp) => resp.error_for_status()?,
            Err(first) => {
                tokio::time::sleep(TRANSPORT_RETRY_PAUSE).await;
                let retried = send().await.map_err(|second| {
                    anyhow::anyhow!("affine graphql: {first}; retry: {second}")
                })?;
                retried.error_for_status()?
            }
        };
        resp.json().await.context("affine graphql response json")
    }
}

fn cut(s: &str, max_chars: usize) -> String {
    s.char_indices()
        .nth(max_chars)
        .map_or_else(|| s.to_string(), |(i, _)| format!("{}…", &s[..i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_truncates_long_strings_with_ellipsis() {
        let long = "a".repeat(300);

        assert_eq!(cut(&long, 200).chars().count(), 201);
        assert!(cut("коротко", 200).ends_with("коротко"));
    }

    /// Контракт docTitle: create_reply обязан нести НЕПУСТОЙ docTitle
    /// в теле запроса — тихий пустой заголовок в снапшоте недопустим.
    #[test]
    fn create_reply_sends_non_empty_doc_title() {
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::ok(
                json!({"data": {"createReply": {"id": "rep-1", "commentId": "c-1"}}}).to_string(),
            ),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let reply_id = ui
                .create_reply("c-1", "ответ агента", "Задача про таймауты")
                .await
                .expect("reply created");

            assert_eq!(reply_id, "rep-1");
            let bodies = mock.request_bodies();
            let graphql = &bodies[1];
            assert!(graphql.contains(r#""docTitle":"Задача про таймауты""#));
            assert!(graphql.contains(r#""insert":"ответ агента""#));
        });
        assert_eq!(mock.request_count(), 2);
    }
}
