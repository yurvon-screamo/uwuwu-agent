//! GitHub DocStore: md-доки uwuwu-hub через локальный git-клон (ленивый
//! fetch). Слаг = путь в docs/ (без .md). Клон — read-кэш и write-рабочая
//! копия одновременно: запись вики (контракт в теле задачи
//! 20261112155302-ttyp35g) — commit + push fast-forward only.
//!
//! Клон-менеджмент (ensure_clone/write_files) — github_doc_clone.rs;
//! поиск — github_doc_search.rs; blob — github_doc_blob.rs.

use super::doc_store::{BlobGet, BlobRef, DocEntry, DocRef, DocStore, FulltextHit, TitleHit};
use super::github_doc_blob::download_attachment;
use super::github_doc_clone::dirs_data_home;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub struct GitHubDocStore {
    pub repo: String,
    pub(super) clone_root: PathBuf,
}

impl GitHubDocStore {
    pub fn new(repo: String, clone_root: PathBuf) -> Self {
        Self { repo, clone_root }
    }

    /// Клон по дефолту: ~/.local/share/uwuwu-cli/hub-clone/<repo-без-слэша>.
    pub fn from_env(repo: String) -> Self {
        assert!(
            !repo.is_empty(),
            "UWUWU_HUB_REPO пуст — doc-хранилище без repo не резолвит клон              (пустой dir_name уводит clone_root в родителя)"
        );
        let root = std::env::var("UWUWU_HUB_CLONE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs_data_home().join("hub-clone"));
        let dir_name = repo.replace('/', "__");
        Self::new(repo, root.join(dir_name))
    }

    pub fn clone_path(&self) -> &Path {
        &self.clone_root
    }

    pub(crate) fn clone_root_str(&self) -> &str {
        self.clone_root.to_str().unwrap_or(".")
    }

    /// Все md-файлы docs/** с данным basename (без .md).
    fn md_by_basename(&self, base: &str) -> Vec<PathBuf> {
        let docs_root = self.clone_path().join("docs");
        let mut out = Vec::new();
        let mut stack = vec![docs_root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && path.file_stem().and_then(|s| s.to_str()) == Some(base)
                {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Файл дока в worktree: slug → docs/<slug>.md (без ..-ходов).
    fn doc_file(&self, slug: &str) -> Result<PathBuf> {
        let safe = slug.trim_matches('/');
        if safe.is_empty() || safe.split('/').any(|seg| seg == "..") {
            bail!("слаг «{slug}» небезопасен");
        }
        Ok(self.clone_root.join("docs").join(format!("{safe}.md")))
    }

    fn asset_file(&self, key: &str) -> Result<PathBuf> {
        let rel = key
            .strip_prefix("assets/")
            .context("ключ blob не assets/")?;
        if rel.split('/').any(|seg| seg == "..") {
            bail!("ключ «{key}» небезопасен");
        }
        Ok(self.clone_root.join("assets").join(rel))
    }
}

fn sanitize_asset_name(name: &str) -> String {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .replace([' ', '#', '?', '%'], "_")
}

#[async_trait::async_trait]
impl DocStore for GitHubDocStore {
    async fn read(&self, doc: &DocRef) -> Result<String> {
        self.ensure_clone().await?;
        let file = self.doc_file(&doc.slug)?;
        match std::fs::read(&file) {
            Ok(bytes) => {
                return Ok(String::from_utf8_lossy(&bytes).to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Фоллбек basename: слаг старой схемы (<proj>/<doc>) мапится в
        // docs/Projects/<proj>/<doc>.md — ищем уникальный файл по basename
        let base = doc.slug.rsplit('/').next().unwrap_or(&doc.slug);
        let candidates = self.md_by_basename(base);
        match candidates.len() {
            1 => {
                let bytes = std::fs::read(&candidates[0])?;
                Ok(String::from_utf8_lossy(&bytes).to_string())
            }
            0 => bail!("док «{}» не найден (docs/**)", doc.slug),
            _ => bail!(
                "док «{}»: неоднозначно, {} кандидатов в docs/** — уточни слаг",
                doc.slug,
                candidates.len()
            ),
        }
    }

    async fn ls(&self, path: &str) -> Result<Vec<DocEntry>> {
        self.ensure_clone().await?;
        let dir = match path.trim_matches('/') {
            "" => self.clone_root.join("docs"),
            clean => {
                if clean.split('/').any(|seg| seg == "..") {
                    bail!("путь «{path}» небезопасен");
                }
                self.clone_root.join("docs").join(clean)
            }
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let is_folder = entry.file_type()?.is_dir();
            out.push(DocEntry {
                name: match is_folder {
                    true => format!("{name}/"),
                    false => name,
                },
                is_folder,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn search_fulltext(&self, query: &str, top_n: usize) -> Result<Vec<FulltextHit>> {
        self.ensure_clone().await?;
        let hits =
            crate::store::github_doc_search::fulltext(self.clone_root_str(), query, top_n).await?;
        Ok(hits)
    }

    async fn search_by_title(&self, query: &str, top_n: usize) -> Result<Vec<TitleHit>> {
        self.ensure_clone().await?;
        crate::store::github_doc_search::by_title(self.clone_root_str(), query, top_n)
    }

    async fn blob_list(&self, prefix: &str) -> Result<Vec<BlobRef>> {
        self.ensure_clone().await?;
        let dir = self
            .clone_root
            .join("assets")
            .join(prefix.trim_matches('/'));
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_type()?.is_file() {
                    out.push(BlobRef {
                        key: format!(
                            "assets/{}",
                            e.path().strip_prefix(&self.clone_root)?.to_string_lossy()
                        ),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn blob_get(&self, key: &BlobRef) -> Result<BlobGet> {
        if key.key.starts_with("https://github.com/user-attachments/") {
            // медиа-аттач из приватного репо: Bearer download (живая проба
            // срез A — бит-в-бит)
            return Ok(BlobGet::Bytes(download_attachment(&key.key).await?));
        }
        self.ensure_clone().await?;
        let path = self.asset_file(&key.key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(BlobGet::Bytes(bytes)),
            Err(_) if !path.exists() => Ok(BlobGet::Missing),
            Err(e) => Err(e.into()),
        }
    }

    async fn blob_put(&self, local_path: &Path, dest_name: &str) -> Result<BlobRef> {
        let bytes = std::fs::read(local_path)
            .with_context(|| format!("локальный файл {} не читается", local_path.display()))?;
        let key = format!("assets/{}", sanitize_asset_name(dest_name));
        let path = self.asset_file(&key)?;
        self.write_files(&[(path, bytes)], &format!("asset: {dest_name}"))
            .await?;
        Ok(BlobRef { key })
    }
}
