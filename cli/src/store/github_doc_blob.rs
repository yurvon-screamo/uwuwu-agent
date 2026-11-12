//! Blob-пути GitHubStore: медиа-аттачи user-attachments (Bearer download —
//! живая проба срез A), произвольные файлы — assets/ в репо через git.

use anyhow::{bail, Context, Result};

/// Download user-attachments URL с Bearer-токеном gh (анонимно — 404,
/// проба срез A: авторизованный download бит-в-бит идентичен).
pub(super) async fn download_attachment(url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://github.com/user-attachments/") {
        bail!("не user-attachments URL: {url}");
    }
    let token = gh_token()?;
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("download аттача: запрос не прошёл")?;
    if !resp.status().is_success() {
        bail!("download аттача: HTTP {} ({url})", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

fn gh_token() -> Result<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("gh auth token не выполнился")?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        bail!("gh auth token пуст — gh не авторизован");
    }
    Ok(token)
}
