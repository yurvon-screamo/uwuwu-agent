//! Вики-репозиторий поверх GitHub (реализация WikiRepo):
//! чтение дока — Contents API, листинг — git trees (recursive),
//! дерево кэшируется в памяти с TTL (листинги идут часто, дерево одно).

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::WikiRepo;

pub struct GitHubWiki {
    repo: String,
    branch: String,
    token: String,
    http: reqwest::Client,
    tree_cache: Mutex<Option<(Instant, Vec<String>)>>,
    ttl: Duration,
}

impl GitHubWiki {
    pub fn new(repo: String, branch: String, token: String) -> Self {
        Self {
            repo,
            branch,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            tree_cache: Mutex::new(None),
            ttl: Duration::from_secs(300),
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
            .map_err(|e| UsecaseError::Board(format!("wiki transport {path}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("wiki body {path}: {e}")))?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Полное дерево docs/**/*.md (кэш с TTL).
    async fn docs_tree(&self) -> UsecaseResult<Vec<String>> {
        {
            let guard = self.tree_cache.lock().expect("tree lock");
            if let Some((at, tree)) = guard.as_ref() {
                if at.elapsed() < self.ttl {
                    return Ok(tree.clone());
                }
            }
        }
        let (status, body) = self
            .get(&format!(
                "/repos/{}/git/trees/{}?recursive=1",
                self.repo, self.branch
            ))
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "wiki tree {}: HTTP {status}",
                self.branch
            )));
        }
        let tree: Vec<String> = body["tree"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| {
                        let path = e["path"].as_str()?;
                        let is_blob = e["type"].as_str()? == "blob";
                        let is_md = path.ends_with(".md");
                        (is_blob && is_md && path.starts_with("docs/")).then(|| {
                            path.trim_start_matches("docs/")
                                .trim_end_matches(".md")
                                .to_string()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        *self.tree_cache.lock().expect("tree lock") = Some((Instant::now(), tree.clone()));
        Ok(tree)
    }
}

#[async_trait]
impl WikiRepo for GitHubWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        if slug.is_empty() {
            return Ok(None);
        }
        let path = format!("docs/{slug}.md");
        let (status, body) = self
            .get(&format!(
                "/repos/{}/contents/{}",
                self.repo,
                encode_path(&path)
            ))
            .await?;
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(UsecaseError::Board(format!("read «{slug}»: HTTP {status}")));
        }
        let b64 = body["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let bytes = base64_decode(&b64)
            .ok_or_else(|| UsecaseError::Board(format!("read «{slug}»: base64")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        let tree = self.docs_tree().await?;
        let norm = prefix.trim_matches('/');
        Ok(tree
            .into_iter()
            .filter(|p| norm.is_empty() || p.starts_with(&format!("{norm}/")) || p == norm)
            .collect())
    }
}

/// Путь в URL: пробелы и не-ascii — percent-encoding (без новой зависимости:
/// кодируруем всё вне безопасного множества).
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Минимальный base64 (стандартный алфавит, без пада-толерантности к мусору).
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in input.bytes() {
        if ch == b'=' {
            break;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_spaces_and_unicode() {
        assert!(encode_path("docs/Projects/uwuwu/Домены.md").contains('%'));
        assert_eq!(encode_path("docs/a/b.md"), "docs/a/b.md");
    }

    #[test]
    fn base64_round_trip() {
        let text = "# Привет, вики\n\nконтент";
        let b64 = {
            // закодируем руками тем же алфавитом
            let bytes = text.as_bytes();
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
                out.push(table_char((n >> 18) & 63));
                out.push(table_char((n >> 12) & 63));
                out.push(if chunk.len() > 1 {
                    table_char((n >> 6) & 63)
                } else {
                    '='
                });
                out.push(if chunk.len() > 2 {
                    table_char(n & 63)
                } else {
                    '='
                });
            }
            out
        };
        assert_eq!(base64_decode(&b64).unwrap(), text.as_bytes());
    }

    fn table_char(i: u32) -> char {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        TABLE[i as usize] as char
    }
}
