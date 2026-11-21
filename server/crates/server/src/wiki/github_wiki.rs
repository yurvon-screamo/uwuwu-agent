//! Вики-репозиторий поверх GitHub (реализация WikiRepo):
//! чтение дока — Contents API, листинг — git trees (recursive),
//! дерево кэшируется в памяти с TTL (листинги идут часто, дерево одно).

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uwuwu_usecase::WikiRepo;
use uwuwu_usecase::{AppendDocOutcome, UsecaseError, UsecaseResult};

pub struct GitHubWiki {
    repo: String,
    branch: String,
    token: String,
    http: reqwest::Client,
    tree_cache: Mutex<Option<(Instant, Vec<String>)>>,
    #[allow(clippy::type_complexity)]
    shas_cache: Mutex<Option<(Instant, Vec<(String, String)>)>>,
    ttl: Duration,
    writer: Option<std::sync::Arc<crate::wiki::gitwiki::GitWikiWriter>>,
}

impl GitHubWiki {
    pub fn with_writer(
        repo: String,
        branch: String,
        token: String,
        writer: Option<std::sync::Arc<crate::wiki::gitwiki::GitWikiWriter>>,
    ) -> Self {
        Self {
            writer,
            repo,
            branch,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            tree_cache: Mutex::new(None),
            shas_cache: Mutex::new(None),
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
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("wiki body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Board(format!("wiki {path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Полное дерево docs/**/*.md (кэш с TTL).
    async fn shas_cache(&self) -> UsecaseResult<Vec<(String, String)>> {
        {
            let guard = self.shas_cache.lock().expect("shas lock");
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
        let list: Vec<(String, String)> = body["tree"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| {
                        let path = e["path"].as_str()?;
                        let sha = e["sha"].as_str()?;
                        let ok = e["type"].as_str()? == "blob"
                            && path.ends_with(".md")
                            && path.starts_with("docs/");
                        ok.then(|| {
                            (
                                path.trim_start_matches("docs/")
                                    .trim_end_matches(".md")
                                    .to_string(),
                                sha.to_string(),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        *self.shas_cache.lock().expect("shas lock") = Some((Instant::now(), list.clone()));
        Ok(list)
    }

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
        let bytes = crate::wiki::gitwiki::base64_std(&b64)
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

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        // то же дерево, но с sha: docs_tree кеширует слаги — sha берём
        // отдельным проходом (кеш общий по HTTP-ответу недоступен,
        // держим собственный короткоживущий кеш)
        self.shas_cache().await
    }

    async fn write_doc(&self, slug: &str, content: &str, message: &str) -> UsecaseResult<()> {
        let Some(w) = &self.writer else {
            return Err(UsecaseError::Board("wiki write не сконфигурирован".into()));
        };
        // инвентаризация кэша сбрасывается: состав изменился
        *self.tree_cache.lock().expect("tree lock") = None;
        w.commit_file(&format!("docs/{slug}.md"), content, message)
            .await
    }

    async fn append_doc(
        &self,
        slug: &str,
        line: &str,
        message: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        let Some(w) = &self.writer else {
            return Err(UsecaseError::Board("wiki write не сконфигурирован".into()));
        };
        w.append_line(&format!("docs/{slug}.md"), line, message)
            .await
    }
}

/// Путь в URL: пробелы и не-ascii — percent-encoding (без новой зависимости:
/// кодируруем всё вне безопасного множества).
// pub(crate) — реюз в asset_store для raw-ссылок ассетов ботов (#1171)
pub(crate) fn encode_path(path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_spaces_and_unicode() {
        assert!(encode_path("docs/Projects/uwuwu/Домены.md").contains('%'));
        assert_eq!(encode_path("docs/a/b.md"), "docs/a/b.md");
    }

    // --- живые пробы (#[ignore]: GH_TOKEN + сеть, read-only) ---

    #[tokio::test]
    #[ignore] // живой прогон: GH_TOKEN + сеть к api.github.com (read-only, #1385)
    async fn docs_ls_live_hub_marks_file_dir_collisions() {
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let wiki = std::sync::Arc::new(GitHubWiki::with_writer(
            "yurvon-screamo/uwuwu-hub".into(),
            "main".into(),
            token,
            None,
        ));
        let view = uwuwu_usecase::DocsView::new(wiki);
        let out = view.list("Projects/uwuwu").await.expect("живой листинг");
        // Живая пара «ADR.md + ADR/» в Projects/uwuwu: строка ADR обязана
        // различимо помечать оба носителя — тихая слепота устранена
        let adr = out
            .iter()
            .find(|l| l.starts_with("Projects/uwuwu/ADR (ambiguous:"))
            .expect("строка ADR с пометкой коллизии в живом листинге");
        assert!(
            adr.contains("file «Projects/uwuwu/ADR.md» + folder «Projects/uwuwu/ADR/»"),
            "оба носителя названы: {adr}"
        );
        // Дети папки без изменений; неколлизионный файл без пометки
        assert!(
            out.iter()
                .any(|l| l.starts_with("Projects/uwuwu/ADR/ADR-001")),
            "дети папки на месте"
        );
        assert!(
            out.iter().any(|l| l == "Projects/uwuwu/README"),
            "неколлизионный файл — голый слаг без пометки"
        );
    }
}
