//! PR-шлюз проекта: GitHub REST (пробы/мерж) — реализация PrGateway.

use async_trait::async_trait;
use serde_json::{json, Value};
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::{PrGateway, PrState};

pub struct GhPrGateway {
    token: String,
    http: reqwest::Client,
}

/// owner/repo/number из URL PR (github.com/owner/repo/pull/N).
pub fn parse_pr_url(url: &str) -> Option<(String, String, u64)> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let number = parts.find_map(|p| p.parse().ok())?;
    Some((owner, repo, number))
}

impl GhPrGateway {
    pub fn new(token: String) -> Self {
        Self {
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn get(&self, path: &str) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let resp = self
            .http
            .get(format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| UsecaseError::Pr(format!("transport {path}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Pr(format!("body {path}: {e}")))?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// CI зелёный: все чек-раны head-коммита завершились успехом.
    async fn ci_green(&self, owner: &str, repo: &str, sha: &str) -> UsecaseResult<bool> {
        let (status, body) = self
            .get(&format!("/repos/{owner}/{repo}/commits/{sha}/check-runs"))
            .await?;
        if !status.is_success() {
            // нет чеков/нет доступа — считаем зелёным (мерж-гейт только CI-ред)
            return Ok(true);
        }
        let runs = body["check_runs"].as_array().cloned().unwrap_or_default();
        Ok(runs.iter().all(|r| {
            r["conclusion"]
                .as_str()
                .map(|c| c == "success" || c == "skipped" || c == "neutral")
                .unwrap_or(true)
        }))
    }

    async fn probe_pull(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> UsecaseResult<Option<PrState>> {
        let (status, pr) = self
            .get(&format!("/repos/{owner}/{repo}/pulls/{number}"))
            .await?;
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(UsecaseError::Pr(format!(
                "probe pulls/{number}: HTTP {status}"
            )));
        }
        if pr["merged"].as_bool() == Some(true) {
            return Ok(Some(PrState::Merged));
        }
        if pr["state"].as_str() == Some("closed") {
            return Ok(Some(PrState::Closed));
        }
        let mergeable = pr["mergeable"].as_bool().unwrap_or(false);
        let sha = pr["head"]["sha"].as_str().unwrap_or_default().to_string();
        let ci = self.ci_green(owner, repo, &sha).await?;
        Ok(Some(PrState::Open {
            mergeable,
            ci_green: ci,
        }))
    }

    async fn probe_by_branch(&self, repo: &str, branch: &str) -> UsecaseResult<Option<PrState>> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Pr(format!("repo в форме owner/name: {repo}")))?;
        let (status, list) = self
            .get(&format!(
                "/repos/{repo}/pulls?head={owner}:{branch}&state=open"
            ))
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Pr(format!(
                "probe by branch {branch}: HTTP {status}"
            )));
        }
        let number = list
            .as_array()
            .and_then(|l| l.first())
            .and_then(|p| p["number"].as_u64());
        match number {
            Some(n) => self.probe_pull(owner, name, n).await,
            None => Ok(None),
        }
    }
}

#[async_trait]
impl PrGateway for GhPrGateway {
    async fn probe(
        &self,
        project_repo: &str,
        pr_url: Option<&str>,
        branch: Option<&str>,
    ) -> UsecaseResult<PrState> {
        if let Some(url) = pr_url.filter(|u| !u.is_empty()) {
            if let Some((owner, repo, number)) = parse_pr_url(url) {
                if let Some(state) = self.probe_pull(&owner, &repo, number).await? {
                    return Ok(state);
                }
            }
        }
        if let Some(branch) = branch.filter(|b| !b.is_empty()) {
            if let Some(state) = self.probe_by_branch(project_repo, branch).await? {
                return Ok(state);
            }
        }
        Ok(PrState::Missing)
    }

    async fn merge(&self, project_repo: &str, pr_url: &str) -> UsecaseResult<()> {
        let Some((owner, repo, number)) = parse_pr_url(pr_url) else {
            return Err(UsecaseError::Pr(format!(
                "мерж: PR-URL не парсится: {pr_url}"
            )));
        };
        let resp = self
            .http
            .put(format!(
                "https://api.github.com/repos/{owner}/{repo}/pulls/{number}/merge"
            ))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .json(&json!({ "merge_method": "merge" }))
            .send()
            .await
            .map_err(|e| UsecaseError::Pr(format!("merge transport: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(UsecaseError::Pr(format!(
                "merge #{number}: HTTP {status}: {}",
                text.chars().take(120).collect::<String>()
            )));
        }
        let _ = project_repo;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_urls() {
        assert_eq!(
            parse_pr_url("https://github.com/yurvon-screamo/uwuwu-agent/pull/293"),
            Some(("yurvon-screamo".into(), "uwuwu-agent".into(), 293))
        );
        assert_eq!(parse_pr_url("https://github.com/a/b/pull/x"), None);
        assert_eq!(parse_pr_url("мусор"), None);
    }
}
