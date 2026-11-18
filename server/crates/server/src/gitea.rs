//! Gitea PR-шлюз: REST /api/v1 (пробы/мерж) — реализация PrGateway (#1269).
//! Проба и мерж — точечно по PR-URL (ячейка — источник истины, ADR-021);
//! веточной пробы у шлюза нет (fm-ветка вторична, project_repo — репо
//! доски в hub, не репо кода задачи). Тесты — gitea_tests.rs.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use uwuwu_usecase::{PrGateway, PrState, UsecaseError, UsecaseResult};

/// host/owner/repo/number из URL PR Gitea-семейства:
/// `<scheme>://<host>/{owner}/{repo}/pulls/{number}`. Строгая форма:
/// 4 сегмента пути, третий — `pulls`, хвост — число; трейлинг `/`
/// допустим; чужие сущности (issues/files) и мусор — None.
pub(crate) fn parse_gitea_pr_url(url: &str) -> Option<(String, String, String, u64)> {
    let rest = url.trim().split_once("://")?.1;
    let (host, path) = rest.trim_end_matches('/').split_once('/')?;
    if host.is_empty() {
        return None;
    }
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pulls" {
        return None;
    }
    let number = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((host.to_string(), owner, repo, number))
}

/// Тело GET /repos/{o}/{r}/pulls/{n} → вердикт (без CI-гейта: конвенция
/// «репо без PR-CI» #447 — ci_green=true). merged → Merged; closed →
/// Closed; mergeable отсутствует или null (инстанс ещё считает) →
/// Pending — симметрия GitHub #1232.
pub(crate) fn classify_pr(body: &Value) -> PrState {
    if body["merged"].as_bool() == Some(true) {
        return PrState::Merged;
    }
    if body["state"].as_str() == Some("closed") {
        return PrState::Closed;
    }
    let Some(mergeable) = body["mergeable"].as_bool() else {
        return PrState::Pending;
    };
    PrState::Open {
        mergeable,
        ci_green: true,
    }
}

/// Ответ PUT /repos/{o}/{r}/pulls/{n}/merge: Ok — мерж принят; Err —
/// сообщение для bounce («мерж не прошёл: …»). 2xx — успех.
pub(crate) fn classify_merge(status: u16, body: &str) -> Result<(), String> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(120).collect());
    Err(if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    })
}

pub struct GiteaPrGateway {
    /// host → токен (env GITEA_TOKENS): инстанс без записи — Foreign без
    /// API-проб (вендор-нейтральный whitelisting, ADR-088).
    tokens: HashMap<String, String>,
    http: reqwest::Client,
}

impl GiteaPrGateway {
    pub fn new(tokens: HashMap<String, String>) -> Self {
        Self {
            tokens,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    /// GET /api/v1{path}: (HTTP-статус, JSON-тело) — None, если тело не
    /// JSON (п.2 Gitea-дока: plain-text 404 = роута нет вообще).
    async fn get(
        &self,
        host: &str,
        token: &str,
        path: &str,
    ) -> UsecaseResult<(u16, Option<Value>)> {
        let resp = self
            .http
            .get(format!("https://{host}/api/v1{path}"))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| UsecaseError::Pr(format!("transport {path}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Pr(format!("body {path}: {e}")))?;
        Ok((status, serde_json::from_str(&text).ok()))
    }
}

#[async_trait]
impl PrGateway for GiteaPrGateway {
    async fn probe(
        &self,
        _project_repo: &str,
        pr_url: Option<&str>,
        _branch: Option<&str>,
    ) -> UsecaseResult<PrState> {
        let Some((host, owner, repo, number)) = pr_url
            .filter(|u| !u.is_empty())
            .and_then(parse_gitea_pr_url)
        else {
            return Ok(PrState::Missing);
        };
        let Some(token) = self.tokens.get(&host) else {
            tracing::info!(
                event = "gitea.foreign",
                host = %host,
                number,
                "Gitea-инстанс без записи в GITEA_TOKENS — Foreign, строка ждёт владельца"
            );
            return Ok(PrState::Foreign);
        };
        let (status, body) = self
            .get(
                &host,
                token,
                &format!("/repos/{owner}/{repo}/pulls/{number}"),
            )
            .await?;
        match status {
            200..=299 => match body {
                Some(body) => Ok(classify_pr(&body)),
                None => {
                    tracing::warn!(
                        event = "gitea.not_json",
                        host = %host,
                        number,
                        "Gitea отдал success без JSON — Foreign"
                    );
                    Ok(PrState::Foreign)
                }
            },
            // JSON-404 — PR нет (ветка есть, bounce «PR не найден» честен);
            // plain-text 404 — роута нет, инстанс API-нечитаем → Foreign
            404 => Ok(if body.is_some() {
                PrState::Missing
            } else {
                tracing::warn!(
                    event = "gitea.no_route",
                    host = %host,
                    number,
                    "Gitea 404 без JSON (роута нет) — Foreign"
                );
                PrState::Foreign
            }),
            401 | 403 => {
                tracing::warn!(
                    event = "gitea.auth",
                    host = %host,
                    number,
                    status,
                    "Gitea отклонил токен — Foreign"
                );
                Ok(PrState::Foreign)
            }
            500..=599 => {
                tracing::warn!(
                    event = "gitea.broken",
                    host = %host,
                    number,
                    status,
                    "Gitea-инстанс отвечает 5xx — Foreign"
                );
                Ok(PrState::Foreign)
            }
            _ => {
                tracing::warn!(
                    event = "gitea.unreadable",
                    host = %host,
                    number,
                    status,
                    "Gitea-ответ не разобран — Foreign"
                );
                Ok(PrState::Foreign)
            }
        }
    }

    async fn merge(&self, _project_repo: &str, pr_url: &str) -> UsecaseResult<()> {
        let Some((host, owner, repo, number)) = parse_gitea_pr_url(pr_url) else {
            return Err(UsecaseError::Pr(format!(
                "мерж: PR-URL не парсится: {pr_url}"
            )));
        };
        let Some(token) = self.tokens.get(&host) else {
            return Err(UsecaseError::Pr(format!(
                "merge #{number}: Gitea-инстанс {host} без токена"
            )));
        };
        let resp = self
            .http
            .put(format!(
                "https://{host}/api/v1/repos/{owner}/{repo}/pulls/{number}/merge"
            ))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .json(&json!({ "Do": "merge" }))
            .send()
            .await
            .map_err(|e| UsecaseError::Pr(format!("merge transport: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        classify_merge(status, &text).map_err(|m| UsecaseError::Pr(format!("merge #{number}: {m}")))
    }
}
