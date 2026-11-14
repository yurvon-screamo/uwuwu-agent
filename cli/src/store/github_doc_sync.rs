//! Sync-семантика общего клон-каталога (задача 1062): ленивый clone/fetch
//! под flock-локом, dirty/ahead-гарды — незакоммиченные правки и
//! незапушенные коммиты никогда не сносятся молча.

use super::git_util::{git, git_raw};
use super::github_doc_lock::clone_lock;
use anyhow::{bail, Result};
use std::time::Duration;

/// Режим sync: Lenient — read-пути (отклонения → warn, читаем как есть);
/// Strict — write-путь (отклонения → громкий отказ).
#[derive(Clone, Copy)]
pub(crate) enum SyncMode {
    Lenient,
    Strict,
}

impl super::github_doc_store::GitHubDocStore {
    /// Read-пути: ленивый sync клона. Dirty/ahead — warn+stale (read-кэшу
    /// свежесть не критична; причина отклонения громкая, не молчаливая).
    pub(crate) async fn ensure_clone(&self) -> Result<()> {
        self.prepare_clone_dir().await?;
        let _guard = clone_lock(self.clone_root_str()).await?;
        self.ensure_clone_locked(SyncMode::Lenient).await
    }

    /// mkdir + идемпотентный git init ДО лока: два параллельных первых
    /// клона оба безвредны (reinit), остальное сериализует лок.
    pub(crate) async fn prepare_clone_dir(&self) -> Result<()> {
        if self.clone_root.join(".git").exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.clone_root)?;
        git(&["-C", self.clone_root_str(), "init", "-q"]).await
    }

    /// Sync под уже взятым локом (ensure_clone / write_files).
    pub(crate) async fn ensure_clone_locked(&self, mode: SyncMode) -> Result<()> {
        if !self.has_origin_ref().await {
            return self.first_checkout_locked().await;
        }
        self.sync_locked(mode).await
    }

    /// Fresh-клон: remote + fetch + первый checkout (гарды не нужны —
    /// ahead-коммитов до первого checkout не существует).
    async fn first_checkout_locked(&self) -> Result<()> {
        let remote_url =
            git_raw(&["-C", self.clone_root_str(), "remote", "get-url", "origin"]).await;
        if remote_url.is_err() {
            let url = format!("https://github.com/{}.git", self.repo);
            git(&["-C", self.clone_root_str(), "remote", "add", "origin", &url]).await?;
        }
        self.fetch_shallow().await?;
        git(&[
            "-C",
            self.clone_root_str(),
            "checkout",
            "-f",
            "-B",
            "main",
            "origin/main",
        ])
        .await
    }

    /// Обычный sync существующего клона: до-fetch снапшот origin/main →
    /// fetch → dirty-гарда → ahead-гарда → reset. Отклонение: Lenient —
    /// warn+skip, Strict — громкий отказ.
    async fn sync_locked(&self, mode: SyncMode) -> Result<()> {
        let before = git_raw(&[
            "-C",
            self.clone_root_str(),
            "rev-parse",
            "refs/remotes/origin/main",
        ])
        .await?
        .trim()
        .to_string();
        self.fetch_shallow().await?;
        if git_status_dirty(self.clone_root_str()).await? {
            return match mode {
                SyncMode::Lenient => {
                    tracing::warn!(
                        "клон {} грязный (незакоммиченные правки) — sync пропущен, \
                         читаю как есть",
                        self.clone_root.display()
                    );
                    Ok(())
                }
                SyncMode::Strict => bail!(
                    "git-клон {} грязный (локальные правки) — CLI-клон чужих правок не \
                     переезжает: разбери вручную",
                    self.clone_root.display()
                ),
            };
        }
        if self.head_drifted(&before).await? {
            return match mode {
                SyncMode::Lenient => {
                    tracing::warn!(
                        "в клоне {} незапушенный коммит (HEAD не на origin/main) — \
                         sync пропущен, читаю как есть",
                        self.clone_root.display()
                    );
                    Ok(())
                }
                SyncMode::Strict => bail!(
                    "git-клон {} содержит незапушенный коммит — reset снёс бы его: \
                     разбери вручную (чей-то незавершённый write)",
                    self.clone_root.display()
                ),
            };
        }
        git(&[
            "-C",
            self.clone_root_str(),
            "reset",
            "--hard",
            "origin/main",
        ])
        .await
    }

    /// Ahead-гарда без топологии: HEAD != до-fetch origin/main → кто-то
    /// закоммитил поверх синкнутого состояния. rev-list/merge-base на
    /// depth-1 shallow недостоверны (graft-граф без merge-base) — сравнение SHA.
    async fn head_drifted(&self, before: &str) -> Result<bool> {
        let head = git_raw(&["-C", self.clone_root_str(), "rev-parse", "HEAD"]).await?;
        Ok(head.trim() != before)
    }

    async fn has_origin_ref(&self) -> bool {
        git(&[
            "-C",
            self.clone_root_str(),
            "rev-parse",
            "--verify",
            "-q",
            "refs/remotes/origin/main",
        ])
        .await
        .is_ok()
    }

    /// `fetch --depth 1` с ретраем shallow-гонок (посторонние git-процессы
    /// вне нашего лока): до 3 попыток, пауза 300/600мс.
    pub(crate) async fn fetch_shallow(&self) -> Result<()> {
        let args = [
            "-C",
            self.clone_root_str(),
            "fetch",
            "--depth",
            "1",
            "origin",
        ];
        let mut attempt: u32 = 0;
        loop {
            match git_raw(&args).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let text = format!("{e:#}");
                    let shallow_race =
                        text.contains("shallow.lock") || text.contains("shallow file has changed");
                    if !shallow_race || attempt >= 2 {
                        return Err(e);
                    }
                    tracing::warn!("shallow-гонка при fetch, ретрай {}: {}", attempt + 1, text);
                    let delay_ms: u64 = 300 * u64::from(attempt + 1);
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                }
            }
        }
    }
}

async fn git_status_dirty(dir: &str) -> Result<bool> {
    let out = git_raw(&["-C", dir, "status", "--porcelain"]).await?;
    Ok(!out.trim().is_empty())
}
