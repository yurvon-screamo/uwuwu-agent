//! GitHub DocStore: md-доки uwuwu-hub через локальный git-клон (ленивый
//! fetch). Слаг = путь в docs/ (без .md). Клон — read-кэш и write-рабочая
//! копия одновременно: запись вики (контракт в теле задачи
//! 20261112155302-ttyp35g) — commit + push fast-forward only.
//!
//! Клон-менеджмент: sync (ensure_clone) — github_doc_sync.rs; write-path
//! (write_files/append/replace) — github_doc_clone.rs; лок —
//! github_doc_lock.rs; поиск — github_doc_search.rs; blob —
//! github_doc_blob.rs.

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

    /// Канон-фоллбек проекта: плоский слаг = имя папки в docs/Projects с
    /// README — детерминированный резолв в README папки (1049: basename-скан
    /// считает заглушку <proj>.md, док-карту <proj>/<proj>.md и вложенные —
    /// ложная неоднозначность, канон-доки не доставались спавнам).
    fn project_readme(&self, slug: &str) -> Option<PathBuf> {
        if slug.contains('/') {
            return None;
        }
        let readme = self
            .clone_path()
            .join("docs")
            .join("Projects")
            .join(slug)
            .join("README.md");
        readme.is_file().then_some(readme)
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

/// Раскладка кандидатов в отказе неоднозначности (паритет task-резолва):
/// пути от корня клона — читаемый перечень для «уточни слаг».
fn candidate_layout(clone_root: &Path, candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|p| {
            p.strip_prefix(clone_root)
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ")
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
        // Канон-фоллбек проекта (1049): плоский слаг с живой папкой
        // docs/Projects/<slug>/README.md — детерминированный README, до
        // basename-скана
        if let Some(readme) = self.project_readme(&doc.slug) {
            let bytes = std::fs::read(&readme)?;
            return Ok(String::from_utf8_lossy(&bytes).to_string());
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
                "док «{}»: неоднозначно, {} кандидатов в docs/** — уточни слаг: {}",
                doc.slug,
                candidates.len(),
                candidate_layout(&self.clone_root, &candidates)
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

#[cfg(test)]
mod doc_read_fallback_tests {
    use super::{candidate_layout, GitHubDocStore};

    fn store_in(tmp: &std::path::Path) -> GitHubDocStore {
        GitHubDocStore::new("o/r".into(), tmp.to_path_buf())
    }

    /// docs/Projects/<proj>/{README.md, <proj>.md} + внешняя заглушка
    /// docs/Projects/<proj>.md — инстанс 1049 (origa: 2 кандидата).
    #[test]
    fn плоский_слаг_проекта_резолвится_в_readme() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("docs/Projects/origa");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("README.md"), "# канон origa").unwrap();
        std::fs::write(proj.join("origa.md"), "# карта ссылок").unwrap();
        std::fs::write(
            tmp.path().join("docs/Projects/origa.md"),
            "# заглушка миграции",
        )
        .unwrap();
        assert_eq!(
            store_in(tmp.path()).project_readme("origa").map(|p| p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()),
            Some("README.md".into())
        );
    }

    #[test]
    fn слаг_с_путём_и_чужие_имена_мимо_фоллбека() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("docs/Projects/ckii");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("README.md"), "x").unwrap();
        let store = store_in(tmp.path());
        assert!(store.project_readme("ckii/Ранбук X").is_none());
        assert!(store.project_readme("нет-такого").is_none());
        assert!(
            store.project_readme("README").is_none(),
            "README — не имя проекта"
        );
    }

    #[test]
    fn папка_без_readme_не_фоллбечится() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs/Projects/пусто")).unwrap();
        assert!(store_in(tmp.path()).project_readme("пусто").is_none());
    }

    #[test]
    fn раскладка_кандидатов_от_корня_клона() {
        let root = std::path::Path::new("/clone");
        let cands = vec![
            root.join("docs/Projects/uwuwu.md"),
            root.join("docs/Projects/uwuwu/uwuwu.md"),
        ];
        assert_eq!(
            candidate_layout(root, &cands),
            "docs/Projects/uwuwu.md, docs/Projects/uwuwu/uwuwu.md"
        );
    }
}
