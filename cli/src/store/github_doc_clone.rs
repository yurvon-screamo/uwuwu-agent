//! Клон-менеджмент GitHub DocStore: ленивый clone/fetch, ff-only write
//! (commit+push, retry после rebase — контракт записи вики).

use super::doc_store::DocStore;
use super::git_util::{git_raw, git_sync_with_author};
use anyhow::{bail, Result};
use std::path::PathBuf;

impl super::github_doc_store::GitHubDocStore {
    pub(crate) async fn ensure_clone(&self) -> Result<()> {
        if !self.clone_root.join(".git").exists() {
            std::fs::create_dir_all(&self.clone_root)?;
            let url = format!("https://github.com/{}.git", self.repo);
            git(&[
                "-C",
                self.clone_root_str(),
                "clone",
                "--depth",
                "1",
                &url,
                ".",
            ])
            .await?;
            return Ok(());
        }
        // fetch + ff reset; dirty worktree — отказ
        if git_status_dirty(self.clone_root_str()).await? {
            bail!(
                "git-клон {} грязный (локальные правки) — CLI-клон чужих правок не \
                 переезжает: разбери вручную",
                self.clone_root.display()
            );
        }
        git(&[
            "-C",
            self.clone_root_str(),
            "fetch",
            "--depth",
            "1",
            "origin",
        ])
        .await?;
        git(&[
            "-C",
            self.clone_root_str(),
            "reset",
            "--hard",
            "origin/main",
        ])
        .await?;
        Ok(())
    }

    /// Запись файлов в клон + commit + push ff-only. Конфликт push (нон-фф)
    /// — fetch/rebase/retry единожды; повторный конфликт — громкий отказ.
    pub(crate) async fn write_files(
        &self,
        files: &[(PathBuf, Vec<u8>)],
        message: &str,
    ) -> Result<()> {
        self.ensure_clone().await?;
        for (path, bytes) in files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        git(&["-C", self.clone_root_str(), "add", "-A"]).await?;
        git_c_user(&["-C", self.clone_root_str(), "commit", "-m", message])?;
        match git_status_ok(&["-C", self.clone_root_str(), "push", "origin", "main"]).await {
            Ok(()) => Ok(()),
            Err(_) => {
                git(&[
                    "-C",
                    self.clone_root_str(),
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                ])
                .await?;
                git(&["-C", self.clone_root_str(), "rebase", "origin/main"]).await?;
                git_status_ok(&["-C", self.clone_root_str(), "push", "origin", "main"]).await?;
                Ok(())
            }
        }
    }
}

async fn git_status_dirty(dir: &str) -> Result<bool> {
    let out = git_raw(&["-C", dir, "status", "--porcelain"]).await?;
    Ok(!out.trim().is_empty())
}

async fn git_status_ok(args: &[&str]) -> Result<()> {
    git_raw(args).await.map(|_| ())
}

async fn git(args: &[&str]) -> Result<()> {
    git_raw(args).await.map(|_| ())
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
