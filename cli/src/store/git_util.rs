//! git-утилиты store-слоя: raw-вызовы git (не gh), автор машинных коммитов.

use anyhow::{bail, Context, Result};
use std::process::Command;

/// Автор машинных коммитов вики (AGENTS) — контракт записи вики.
const GIT_AUTHOR: [&str; 4] = [
    "-c",
    "user.name=AGENTS",
    "-c",
    "user.email=agents@uwuwu.net",
];

/// Синхронный git с автором машинных коммитов (commit-путь).
pub fn git_sync_with_author(args: &[&str]) -> Result<()> {
    let mut full = vec!["git"];
    full.extend_from_slice(&GIT_AUTHOR);
    full.extend_from_slice(args);
    let out = Command::new(full[0])
        .args(&full[1..])
        .output()
        .context("git не запустился")?;
    if !out.status.success() {
        bail!(
            "git [{}] упал: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Асинхронный git-мутатор без чтения stdout (fetch/reset/checkout/…).
pub async fn git(args: &[&str]) -> Result<()> {
    git_raw(args).await.map(|_| ())
}

/// Асинхронный raw git (stdout строкой; stderr при неуспехе — в ошибку).
pub async fn git_raw(args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .output()
        .await
        .context("git не запустился")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        bail!(
            "git [{}] упал: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(stdout)
}
