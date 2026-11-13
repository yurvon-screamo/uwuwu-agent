//! Write-path доков (docs create/append/replace/delete/tree): write-
//! транзакция поверх git-конвейера записи. Слаг — только строгая
//! doc_file-резолюция (basename-фоллбек read в write не переезжает).
//! Контракт --base: sha256 от stdout `docs read` (байт-в-байт равен файлу
//! клона); расхождение — громкий отказ. Задача 46.
//! Текстовые гварды входа — github_doc_guards.rs.

use super::doc_store::DocWrite;
use super::github_doc_guards::{create_bytes, create_warnings, reject_frontmatter, sha256_hex};
use super::github_doc_store::GitHubDocStore;
use anyhow::{bail, Result};
use std::path::PathBuf;

impl GitHubDocStore {
    async fn write_doc_bytes(
        &self,
        slug: &str,
        files: &[(PathBuf, Vec<u8>)],
        deleted: &[PathBuf],
        action: &str,
    ) -> Result<()> {
        let msg = format!("wiki: {slug} (docs {action})");
        self.write_files(files, deleted, &msg).await
    }

    /// Создать док: шапку ведёт store (frontmatter + H1 из тайтла слага),
    /// вход — тело после шапки. Гварды: существует/frontmatter/path-safe;
    /// ADR-номер занят / новая папка — warn, не отказ.
    pub(crate) async fn create_doc(&self, slug: &str, content: &str) -> Result<DocWrite> {
        self.ensure_clone().await?;
        let file = self.doc_file(slug)?;
        if file.exists() {
            bail!("док «{slug}» существует — read/replace, создание не нужно");
        }
        let title = super::github_doc_guards::split_slug(slug).1;
        reject_frontmatter(content)?;
        let warnings = create_warnings(self, slug);
        let bytes = create_bytes(&title, content);
        self.write_doc_bytes(slug, &[(file, bytes.clone())], &[], "create")
            .await?;
        // финальный контент известен из входа — без повторного read
        // (лишний fetch на клоне)
        Ok(DocWrite {
            doc: String::from_utf8_lossy(&bytes).to_string(),
            warnings,
        })
    }

    /// Дописать в конец существующего дока (дока нет → отказ).
    pub(crate) async fn append_doc(&self, slug: &str, addition: &str) -> Result<String> {
        self.ensure_clone().await?;
        let file = self.doc_file(slug)?;
        if !file.exists() {
            bail!("док «{slug}» не найден — создание только через docs create");
        }
        let current = std::fs::read_to_string(&file)?;
        let mut next = current.clone();
        if !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(addition.trim_end_matches('\n'));
        next.push('\n');
        if next == current {
            return Ok(next);
        }
        self.write_doc_bytes(slug, &[(file, next.clone().into_bytes())], &[], "append")
            .await?;
        Ok(next)
    }

    /// Перезаписать док контентом входа (base — sha256 read-экспорта).
    pub(crate) async fn replace_doc(
        &self,
        slug: &str,
        body: &str,
        base: Option<&str>,
    ) -> Result<String> {
        self.ensure_clone().await?;
        let file = self.doc_file(slug)?;
        if !file.exists() {
            bail!("док «{slug}» не найден — replace не создаёт, см. docs create");
        }
        let current = std::fs::read(&file)?;
        if let Some(expected) = base {
            let actual = sha256_hex(&current);
            if actual != expected.trim() {
                bail!(
                    "док «{slug}» менялся с момента чтения (sha256 {actual} ≠ base {expected}) — перечитай docs read и повтори"
                );
            }
        }
        if current == body.as_bytes() {
            return Ok(body.to_string());
        }
        self.write_doc_bytes(slug, &[(file, body.as_bytes().to_vec())], &[], "replace")
            .await?;
        Ok(body.to_string())
    }

    /// Снести док-файл (папка → отказ).
    pub(crate) async fn delete_doc(&self, slug: &str) -> Result<()> {
        self.ensure_clone().await?;
        let dir = self.clone_path().join("docs").join(slug.trim_matches('/'));
        if dir.is_dir() {
            bail!("«{slug}» — папка: снеси доки по одному, каскад-гварда нет");
        }
        let file = self.doc_file(slug)?;
        if !file.exists() {
            bail!("док «{slug}» не найден");
        }
        self.write_doc_bytes(slug, &[], &[file], "delete").await
    }

    /// Все md-слаги клона, отсортированные построчно.
    pub(crate) async fn tree(&self) -> Result<Vec<String>> {
        self.ensure_clone().await?;
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
                } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let rel = path
                        .strip_prefix(&docs_root)?
                        .to_string_lossy()
                        .trim_end_matches(".md")
                        .to_string();
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Существует ли док по строгому пути, по текущему клону (без
    /// basename-фоллбека; fetch мутация делает сама — кратные
    /// ensure_clone на команду не заводим).
    pub(crate) async fn exists(&self, slug: &str) -> Result<bool> {
        Ok(self.doc_file(slug)?.exists())
    }
}
