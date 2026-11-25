//! Свидетель существования на miss-пути Contents API (#1653): свежее
//! дерево git и blob по sha. Contents отдаёт 404 после git-data-коммита
//! дольше, чем дерево видит файл; вердикты Created/AppendMissing не
//! должны выноситься по устаревшему чтению.

use reqwest::{Client, Method};
use serde_json::Value;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

use crate::wiki::encode_path;
use crate::wiki::gitwiki::base64_std;

/// Чтение по дереву: только после того, как Contents ответил «нет».
pub(crate) struct TreeWitness<'a> {
    pub http: &'a Client,
    pub token: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
}

impl TreeWitness<'_> {
    /// None — файла нет и в дереве; Err — свидетельство недоступно
    /// (тогда «нет» не доказано и мутация обязана отказать честно,
    /// а не портить шапку/текст).
    pub(crate) async fn read(&self, path: &str) -> UsecaseResult<Option<String>> {
        let tree_path = format!(
            "/repos/{}/git/trees/{}?recursive=1",
            self.repo,
            encode_path(self.branch)
        );
        let (status, tree) = self.call(Method::GET, &tree_path).await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "wiki tree {}: HTTP {status}",
                self.branch
            )));
        }
        let Some(sha) = blob_sha_for(&tree, path)? else {
            return Ok(None);
        };
        let blob_path = format!("/repos/{}/git/blobs/{sha}", self.repo);
        let (status, blob) = self.call(Method::GET, &blob_path).await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "wiki blob {path}: HTTP {status}"
            )));
        }
        let b64 = blob["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64_std(&b64)
            .ok_or_else(|| UsecaseError::Board(format!("wiki blob {path}: base64")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let resp = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("wiki witness {path}: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("wiki witness body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Board(format!("wiki witness {path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }
}

/// Blob-sha документа в свежем дереве. Усечённое дерево не доказывает
/// отсутствие: такой ответ — честная ошибка вместо вердикта Created.
pub(crate) fn blob_sha_for<'a>(tree: &'a Value, path: &str) -> UsecaseResult<Option<&'a str>> {
    if tree["truncated"].as_bool().unwrap_or(false) {
        return Err(UsecaseError::Board(format!(
            "wiki tree truncated — cannot confirm absence of {path}"
        )));
    }
    let found = tree["tree"]
        .as_array()
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry["path"].as_str() == Some(path) && entry["type"].as_str() == Some("blob")
            })
        })
        .and_then(|entry| entry["sha"].as_str());
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn witness_finds_blob_sha_by_exact_path() {
        let tree = json!({"tree": [
            {"path": "docs/Experience/a.md", "type": "blob", "sha": "other"},
            {"path": "docs/Experience/a/b.md", "type": "blob", "sha": "wanted"},
            {"path": "docs/Experience/a/b.md", "type": "tree", "sha": "not-a-blob"}
        ]});
        assert_eq!(
            blob_sha_for(&tree, "docs/Experience/a/b.md").expect("tree"),
            Some("wanted")
        );
    }

    #[test]
    fn witness_reports_absence_when_path_missing() {
        let tree = json!({"tree": [{"path": "docs/other.md", "type": "blob", "sha": "x"}]});
        assert_eq!(blob_sha_for(&tree, "docs/missing.md").expect("tree"), None);
    }

    #[test]
    fn truncated_tree_cannot_witness_absence() {
        let tree = json!({"truncated": true, "tree": []});
        assert!(blob_sha_for(&tree, "docs/missing.md").is_err());
    }
}
