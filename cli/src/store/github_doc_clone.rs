//! Write-путь клон-менеджмента: commit + push ff-only под flock-локом
//! (нулевое окно commit→push для cli-писателей), атомарная запись файлов.
//! Sync-семантика (ensure_clone) — github_doc_sync.rs; лок —
//! github_doc_lock.rs.

use super::git_util::{git, git_raw, git_sync_with_author};
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
        deleted: &[PathBuf],
        message: &str,
    ) -> Result<()> {
        self.prepare_clone_dir().await?;
        let _guard = clone_lock(self.clone_root_str()).await?;
        // Свой мусор от крэша чистим ДО гарды — иначе untracked .uwuwu-tmp
        // валит dirty-проверку и write отказывает сам на себе.
        cleanup_stale_tmp(Path::new(self.clone_root_str()))?;
        self.ensure_clone_locked(super::github_doc_sync::SyncMode::Strict)
            .await?;
        for (path, bytes) in files {
            write_file_atomic(path, bytes)?;
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
/// видит обрезанный файл посреди чужой записи. Родители создаются —
/// «дока нет — не падай» (append в новый путь легитимен).
fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .context("путь записи без имени файла")?
        .to_string_lossy();
    let tmp = path.with_file_name(format!(".{file_name}.uwuwu-tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Снести свой мусор от крэша между tmp-write и rename (`.{name}.uwuwu-tmp`):
/// иначе следующий `git add -A` утащит его в вики.
fn cleanup_stale_tmp(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != ".git") {
                    stack.push(path);
                }
            } else {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && name.ends_with(".uwuwu-tmp") {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
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
