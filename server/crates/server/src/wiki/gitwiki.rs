//! Запись в вики поверх GitHub Git Data API (blob→tree→commit→ref):
//! ff-only (update_ref без force), append-семантика с read-modify-write.
//! Единый писатель — сервер (конвейер), конфликт ff громко отвергается.

use serde_json::{json, Value};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct GitWikiWriter {
    repo: String,
    branch: String,
    token: String,
    http: reqwest::Client,
}

impl GitWikiWriter {
    pub fn new(repo: String, branch: String, token: String) -> Self {
        Self {
            repo,
            branch,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn call(
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
            .map_err(|e| UsecaseError::Board(format!("gitwiki transport {path}: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("gitwiki body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(Self::err(format!("{path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    fn err(msg: String) -> UsecaseError {
        UsecaseError::Board(format!("gitwiki: {msg}"))
    }

    /// Читает текущее содержимое файла (None — файла нет).
    pub async fn read(&self, path: &str) -> UsecaseResult<Option<String>> {
        let (status, body) = self
            .call(
                reqwest::Method::GET,
                &format!("/repos/{}/contents/{}", self.repo, path),
                None,
            )
            .await?;
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Self::err(format!("read {path}: HTTP {status}")));
        }
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64_std(&b64).ok_or_else(|| Self::err(format!("read {path}: base64")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Коммит файла с сообщением (ff-only на branch).
    pub async fn commit_file(&self, path: &str, content: &str, message: &str) -> UsecaseResult<()> {
        // ref → commit → base tree
        let (_, head) = self
            .call(
                reqwest::Method::GET,
                &format!("/repos/{}/git/ref/heads/{}", self.repo, self.branch),
                None,
            )
            .await?;
        let head_sha = head["object"]["sha"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if head_sha.is_empty() {
            return Err(Self::err(format!("нет ref heads/{}", self.branch)));
        }
        let (_, commit) = self
            .call(
                reqwest::Method::GET,
                &format!("/repos/{}/git/commits/{head_sha}", self.repo),
                None,
            )
            .await?;
        let base_tree = commit["tree"]["sha"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // blob → tree → commit → ref (ff)
        let (_, blob) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/blobs", self.repo),
                Some(json!({ "content": content, "encoding": "utf-8" })),
            )
            .await?;
        let blob_sha = blob["sha"].as_str().unwrap_or_default().to_string();
        let (_, tree) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/trees", self.repo),
                Some(json!({
                    "base_tree": base_tree,
                    "tree": [{ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha }]
                })),
            )
            .await?;
        let tree_sha = tree["sha"].as_str().unwrap_or_default().to_string();
        let (_, new_commit) = self
            .call(
                reqwest::Method::POST,
                &format!("/repos/{}/git/commits", self.repo),
                Some(json!({ "message": message, "tree": tree_sha, "parents": [head_sha] })),
            )
            .await?;
        let new_sha = new_commit["sha"].as_str().unwrap_or_default().to_string();
        let (status, resp) = self
            .call(
                reqwest::Method::PATCH,
                &format!("/repos/{}/git/refs/heads/{}", self.repo, self.branch),
                Some(json!({ "sha": new_sha })),
            )
            .await?;
        let fast_forward =
            status.is_success() && resp["object"]["sha"].as_str() == Some(new_sha.as_str());
        if !fast_forward {
            return Err(Self::err(format!(
                "update_ref {path}: не ff (HTTP {status}) — конкурирующий коммит"
            )));
        }
        Ok(())
    }

    /// Append строки в конец файла (создаёт при отсутствии).
    pub async fn append_line(&self, path: &str, line: &str, message: &str) -> UsecaseResult<()> {
        let current = self.read(path).await?;
        let next = match current {
            Some(mut c) => {
                if !c.ends_with('\n') {
                    c.push('\n');
                }
                c.push_str(line);
                c.push('\n');
                c
            }
            None => format!("{line}\n"),
        };
        self.commit_file(path, &next, message).await
    }
}

/// Стандартный base64-декодер (алфавит + /, с паддингом).
pub fn base64_std(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in input.bytes() {
        if ch == b'=' || ch == b'\n' {
            continue;
        }
        let val = TABLE.iter().position(|&t| t == ch)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}
