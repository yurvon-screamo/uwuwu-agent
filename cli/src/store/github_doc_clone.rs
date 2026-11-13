//! Клон-менеджмент GitHub DocStore: ленивый clone/fetch, ff-only write
//! (commit+push, retry после rebase — контракт записи вики).

use super::git_util::{git_raw, git_sync_with_author};
use anyhow::{bail, Result};
use std::path::PathBuf;

impl super::github_doc_store::GitHubDocStore {
    pub(crate) async fn ensure_clone(&self) -> Result<()> {
        if !self.clone_root.join(".git").exists() {
            std::fs::create_dir_all(&self.clone_root)?;
            let url = format!("https://github.com/{}.git", self.repo);
            // Каталог может быть не пуст (мигратор выгрузил docs до клона) —
            // init + remote + fetch + checkout -f поверх существующих файлов
            git(&["-C", self.clone_root_str(), "init", "-q"]).await?;
            git(&["-C", self.clone_root_str(), "remote", "add", "origin", &url]).await?;
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
                "checkout",
                "-f",
                "-B",
                "main",
                "origin/main",
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

    /// Запись файлов в клон + commit + push ff-only. add — точечный по
    /// целевым путям (чужой незакоммиченный мусор клона уехать не может —
    /// класс инцидента b46b8fd; ensure_clone с dirty-гардом зовёт caller
    /// ДО записи). Конфликт push (нон-фф) — fetch/rebase/retry единожды;
    /// повторный конфликт — громкий отказ.
    pub(crate) async fn write_files(
        &self,
        files: &[(PathBuf, Vec<u8>)],
        deleted: &[PathBuf],
        message: &str,
    ) -> Result<()> {
        for (path, bytes) in files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        for path in deleted {
            std::fs::remove_file(path)?;
        }
        let mut add: Vec<String> = files
            .iter()
            .map(|(p, _)| p.to_string_lossy().to_string())
            .chain(deleted.iter().map(|p| p.to_string_lossy().to_string()))
            .collect();
        add.sort();
        add.dedup();
        let mut add_args: Vec<String> = vec![
            "-C".into(),
            self.clone_root_str().into(),
            "add".into(),
            "--".into(),
        ];
        add_args.extend(add);
        let add_refs: Vec<&str> = add_args.iter().map(String::as_str).collect();
        git_raw(&add_refs).await?;
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
