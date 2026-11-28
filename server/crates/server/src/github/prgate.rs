//! PR-шлюз проекта: GitHub REST (пробы/мерж) — реализация PrGateway.

use super::http_timeout;
use async_trait::async_trait;
use serde_json::{json, Value};
use uwuwu_usecase::{PrGateway, PrProbe, PrState};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct GhPrGateway {
    token: String,
    http: reqwest::Client,
}

/// owner/repo/number из URL PR (github.com/owner/repo/pull/N).
/// Строгая форма PR-URL: literal github.com/{owner}/{repo}/pull/{number}.
/// Остальные http(s)-URL (issues/blob/чужой-вендор) — None: probe/merge
/// не пробуют чужие сущности по числовым сегментам пути (#1155).
pub fn parse_pr_url(url: &str) -> Option<(String, String, u64)> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pull" {
        return None;
    }
    let number = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((owner, repo, number))
}

/// Чек-раны → вердикт: Some(true) зелёный, Some(false) красный, None —
/// есть незавершённые раны (pending). Пусто (CI выключен/не настроен) —
/// зелёный: мерж-гейт только CI-red. Ран без conclusion (queued/
/// in_progress) — pending, не зелёный (#1232: прежний `unwrap_or(true)`
/// пропускал мерж при бегущих чеках).
fn classify_check_runs(runs: &[Value]) -> Option<bool> {
    if runs.is_empty() {
        return Some(true);
    }
    let mut any_red = false;
    for r in runs {
        match r["conclusion"].as_str() {
            Some("success") | Some("skipped") | Some("neutral") => {}
            Some(_) => any_red = true,
            None => return None,
        }
    }
    Some(!any_red)
}

impl GhPrGateway {
    pub fn new(token: String) -> Self {
        Self {
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(http_timeout())
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
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Pr(format!("body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Pr(format!("pr gate {path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// CI-вердикт по чек-ранам head-коммита: Some(true) — зелёный,
    /// Some(false) — красный, None — есть незавершённые раны (pending).
    async fn ci_verdict(&self, owner: &str, repo: &str, sha: &str) -> UsecaseResult<Option<bool>> {
        let (status, body) = self
            .get(&format!("/repos/{owner}/{repo}/commits/{sha}/check-runs"))
            .await?;
        if !status.is_success() {
            // 403 сюда не доходит — gherr::classify выше отдаёт громкий Err
            // (rate limit/permission); ветка осмысленна для 404 и прочих
            // не-success: нет чеков — считаем зелёным (мерж-гейт только CI-ред)
            return Ok(Some(true));
        }
        let runs = body["check_runs"].as_array().cloned().unwrap_or_default();
        Ok(classify_check_runs(&runs))
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
        // mergeable=null — GitHub ещё считает: pending, не ложный конфликт
        // (#1232: прежний `unwrap_or(false)` давал ложный bounce)
        let Some(mergeable) = pr["mergeable"].as_bool() else {
            return Ok(Some(PrState::Pending));
        };
        let sha = pr["head"]["sha"].as_str().unwrap_or_default().to_string();
        let Some(ci_green) = self.ci_verdict(owner, repo, &sha).await? else {
            return Ok(Some(PrState::Pending));
        };
        Ok(Some(PrState::Open {
            mergeable,
            ci_green,
        }))
    }

    async fn probe_by_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> UsecaseResult<Option<(u64, PrState)>> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Pr(format!("repo в форме owner/name: {repo}")))?;
        // state=all, не open (#1519, ревью Common): смерженный руками PR при
        // пустой/протухшей ячейке обязан классифицироваться Merged — no-op
        // «строка ждёт владельца», а не bounce «PR не найден» (шторм-кейс
        // #1424: тики стоят, владелец мержит руками)
        let (status, list) = self
            .get(&format!(
                "/repos/{repo}/pulls?head={owner}:{branch}&state=all"
            ))
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Pr(format!(
                "probe by branch {branch}: HTTP {status}"
            )));
        }
        let items: Vec<(u64, bool)> = list
            .as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|p| {
                        let number = p["number"].as_u64()?;
                        let state = p["state"].as_str() == Some("open");
                        Some((number, state))
                    })
                    .collect()
            })
            .unwrap_or_default();
        match pick_branch_pr(&items) {
            Some(n) => Ok(self.probe_pull(owner, name, n).await?.map(|s| (n, s))),
            None => Ok(None),
        }
    }
}

/// Выбор PR веточной пробы из ответа state=all (#1519, ревью Common):
/// открытый предпочтительнее закрытых/смерженных (ветка могла пережить
/// закрытый PR и получить новый); иначе первый из списка — свежайший.
/// Чистая функция — пин классификации без HTTP (ADR-110).
fn pick_branch_pr(items: &[(u64, bool)]) -> Option<u64> {
    items
        .iter()
        .find(|(_, open)| *open)
        .or_else(|| items.first())
        .map(|(n, _)| *n)
}

#[async_trait]
impl PrGateway for GhPrGateway {
    async fn probe(
        &self,
        project_repo: &str,
        pr_url: Option<&str>,
        branch: Option<&str>,
    ) -> UsecaseResult<PrProbe> {
        if let Some(url) = pr_url.filter(|u| !u.is_empty()) {
            if let Some((owner, repo, number)) = parse_pr_url(url) {
                if let Some(state) = self.probe_pull(&owner, &repo, number).await? {
                    return Ok(PrProbe {
                        state,
                        url: Some(url.to_string()),
                    });
                }
            }
        }
        if let Some(branch) = branch.filter(|b| !b.is_empty()) {
            if let Some((number, state)) = self.probe_by_branch(project_repo, branch).await? {
                return Ok(PrProbe {
                    state,
                    url: Some(format!("https://github.com/{project_repo}/pull/{number}")),
                });
            }
        }
        Ok(PrProbe {
            state: PrState::Missing,
            url: None,
        })
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
        let headers = resp.headers().clone();
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Pr(format!("merge #{number}: {fail}")));
        }
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
    fn pick_branch_pr_prefers_open_over_closed() {
        // #1519, ревью Common: state=all — открытый PR предпочтительнее
        // закрытых/смерженных предшественников той же ветки
        assert_eq!(pick_branch_pr(&[(7, false), (9, true)]), Some(9));
        assert_eq!(pick_branch_pr(&[(9, true), (7, false)]), Some(9));
        // открытых нет — первый из списка (свежайший смерженный/закрытый)
        assert_eq!(pick_branch_pr(&[(7, false)]), Some(7));
        assert_eq!(pick_branch_pr(&[]), None);
    }

    #[test]
    fn parses_pr_urls() {
        assert_eq!(
            parse_pr_url("https://github.com/yurvon-screamo/uwuwu-agent/pull/293"),
            Some(("yurvon-screamo".into(), "uwuwu-agent".into(), 293))
        );
        assert_eq!(parse_pr_url("https://github.com/a/b/pull/x"), None);
        assert_eq!(parse_pr_url("мусор"), None);
    }

    #[test]
    fn classify_check_runs_empty_is_green() {
        // CI выключен/не настроен — мерж-гейт только CI-red (#1232,
        // биллинг Actions снят: чеков нет, автомерж не молчит)
        assert_eq!(classify_check_runs(&[]), Some(true));
    }

    #[test]
    fn classify_check_runs_success_skipped_neutral_is_green() {
        let runs = vec![
            json!({ "conclusion": "success" }),
            json!({ "conclusion": "skipped" }),
            json!({ "conclusion": "neutral" }),
        ];
        assert_eq!(classify_check_runs(&runs), Some(true));
    }

    #[test]
    fn classify_check_runs_failure_is_red() {
        let runs = vec![
            json!({ "conclusion": "success" }),
            json!({ "conclusion": "failure" }),
        ];
        assert_eq!(classify_check_runs(&runs), Some(false));
    }

    #[test]
    fn classify_check_runs_unfinished_is_pending() {
        // queued/in_progress — ран без conclusion: вердикт следующего
        // прогона, не зелёный (баг #1232: unwrap_or(true) пропускал мерж)
        let runs = vec![
            json!({ "conclusion": "success" }),
            json!({ "status": "in_progress" }),
        ];
        assert_eq!(classify_check_runs(&runs), None);
        let queued = vec![json!({ "status": "queued", "conclusion": null })];
        assert_eq!(classify_check_runs(&queued), None);
    }

    #[test]
    fn rejects_non_pull_urls() {
        // #1155: артефакт/чужие сущности не пробуются как PR
        assert_eq!(
            parse_pr_url("https://github.com/a/b/issues/52"),
            None,
            "issue-URL — не PR"
        );
        assert_eq!(
            parse_pr_url("https://github.com/a/b/blob/main/docs/x.md"),
            None,
            "вики-артефакт"
        );
        assert_eq!(
            parse_pr_url("https://github.com/a/b/blob/123/x"),
            None,
            "число в пути без pull больше не парсится"
        );
        assert_eq!(
            parse_pr_url("https://git.true-wash.com/a/b/pulls/9"),
            None,
            "чужой вендор"
        );
        assert_eq!(
            parse_pr_url("https://github.com/a/b/pull/293/files"),
            None,
            "хвост за номером — не canonical"
        );
    }
}
