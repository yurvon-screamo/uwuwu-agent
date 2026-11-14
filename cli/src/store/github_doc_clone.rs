//! Write-путь клон-менеджмента: commit + push ff-only под flock-локом
//! (нулевое окно commit→push для cli-писателей), атомарная запись файлов.
//! Sync-семантика (ensure_clone) — github_doc_sync.rs; лок —
//! github_doc_lock.rs.

use super::doc_store::DocStore;
use super::git_util::{git, git_sync_with_author};
use super::github_doc_lock::clone_lock;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

impl super::github_doc_store::GitHubDocStore {
    /// Запись файлов в клон + commit + push ff-only — весь сегмент под
    /// локом: sync strict-режимом (dirty/ahead → громкий отказ) не даёт
    /// снести чужую правку/коммит, чужой sync не вклинивается между
    /// check и reset. Конфликт push (нон-фф) — fetch/rebase/retry
    /// единожды; повторный конфликт — громкий отказ.
    pub(crate) async fn write_files(
        &self,
        files: &[(PathBuf, Vec<u8>)],
        message: &str,
    ) -> Result<()> {
        self.prepare_clone_dir().await?;
        let _guard = clone_lock(self.clone_root_str()).await?;
        self.ensure_clone_locked(super::github_doc_sync::SyncMode::Strict)
            .await?;
        for (path, bytes) in files {
            write_file_atomic(path, bytes)?;
        }
        git(&["-C", self.clone_root_str(), "add", "-A"]).await?;
        git_c_user(&["-C", self.clone_root_str(), "commit", "-m", message])?;
        match git(&["-C", self.clone_root_str(), "push", "origin", "main"]).await {
            Ok(()) => Ok(()),
            Err(_) => {
                self.fetch_shallow().await?;
                git(&["-C", self.clone_root_str(), "rebase", "origin/main"]).await?;
                git(&["-C", self.clone_root_str(), "push", "origin", "main"]).await?;
                Ok(())
            }
        }
    }
}

/// Запись через tmp-файл + rename в том же каталоге: reader вне лока не
/// видит обрезанный файл посреди чужой записи.
fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .context("путь записи без имени файла")?
        .to_string_lossy();
    let tmp = path.with_file_name(format!(".{file_name}.uwuwu-tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn git_c_user(args: &[&str]) -> Result<()> {
    git_sync_with_author(args)
}

pub(super) fn dirs_data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/share")
        })
        .join("uwuwu-cli")
}

impl super::github_doc_store::GitHubDocStore {
    /// Дописать в конец дока (person append): read-modify-write ff-push.
    pub(crate) async fn append_doc(&self, slug: &str, addition: &str) -> Result<()> {
        let current = self
            .read(&super::doc_store::DocRef {
                slug: slug.to_string(),
            })
            .await
            .unwrap_or_default();
        let mut next = current;
        if !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(addition);
        next.push('\n');
        self.write_doc(slug_if_doc(slug), next).await
    }

    /// Перезаписать док целиком (person replace).
    pub(crate) async fn replace_doc(&self, slug: &str, body: &str) -> Result<()> {
        self.write_doc(slug_if_doc(slug), body.to_string()).await
    }

    async fn write_doc(&self, slug: String, content: String) -> Result<()> {
        let file = self.clone_path().join("docs").join(format!("{slug}.md"));
        self.write_files(
            &[(file, content.into_bytes())],
            &format!("wiki: {slug} (person)"),
        )
        .await
    }
}

fn slug_if_doc(slug: &str) -> String {
    slug.to_string()
}
