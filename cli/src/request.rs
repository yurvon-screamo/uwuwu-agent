use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use crate::request_util::{compact_timestamp, meta_from_path, resolve_request_file, slugify};

pub const REQUEST_ACTIONS: [&str; 3] = ["create", "update", "delete"];

pub use crate::request_util::RequestMeta;

#[derive(Subcommand)]
pub enum RequestSub {
    /// List pending change requests.
    List,
    /// Read a request's full content.
    Get {
        #[arg(help = "Request id (filename stem, see `list`)")]
        id: String,
    },
    /// Discard a request without applying.
    Delete {
        #[arg(help = "Request id (filename stem, see `list`)")]
        id: String,
    },
}

/// Параметры быстрого создания заявки (топ-режим `uwuwu-cli request "…" `):
/// текст позиционным аргументом или --stdin — симметрия с `uwuwu-cli facts`.
pub struct QuickCreate<'a> {
    pub text: Vec<&'a str>,
    pub stdin: bool,
    pub action: &'a str,
    pub target: &'a str,
    pub reason: &'a str,
}

pub fn handle(
    command: Option<RequestSub>,
    quick: Option<QuickCreate<'_>>,
    requests_dir: &Path,
) -> Result<()> {
    match command {
        Some(RequestSub::List) => {
            let entries = list_requests(requests_dir)?;
            if entries.is_empty() {
                println!("No requests.");
                return Ok(());
            }
            for e in &entries {
                println!("{}  [{}] {} — {}", e.created, e.action, e.target, e.reason);
                println!("    id: {}", e.id);
            }
            println!("\n{} request(s)", entries.len());
        }
        Some(RequestSub::Get { id }) => {
            let content = get_request(&id, requests_dir)?;
            println!("{content}");
        }
        Some(RequestSub::Delete { id }) => {
            delete_request(&id, requests_dir)?;
            println!("Request discarded: {id}");
        }
        None if quick.is_none() => {
            // голый `uwuwu-cli request` → list (дефолт-просмотр очереди)
            let entries = list_requests(requests_dir)?;
            if entries.is_empty() {
                println!("No requests.");
                return Ok(());
            }
            for e in &entries {
                println!("{}  [{}] {} — {}", e.created, e.action, e.target, e.reason);
                println!("    id: {}", e.id);
            }
            println!("\n{} request(s)", entries.len());
        }
        None => {
            let q = quick.expect("quick create args");
            let content = if q.stdin {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else {
                q.text.join(" ")
            };
            let path = create_request(q.action, q.target, &content, q.reason, requests_dir)?;
            println!("Request saved: {}", path.display());
        }
    }
    Ok(())
}

pub fn create_request(
    action: &str,
    target: &str,
    content: &str,
    reason: &str,
    requests_dir: &Path,
) -> Result<PathBuf> {
    if !REQUEST_ACTIONS.contains(&action) {
        bail!("invalid action '{action}': expected create | update | delete");
    }
    if target.trim().is_empty() {
        bail!("target is empty");
    }
    if (action == "create" || action == "update") && content.trim().is_empty() {
        bail!("content is required for {action} requests");
    }

    std::fs::create_dir_all(requests_dir)?;

    let ts = compact_timestamp();
    let slug = slugify(format!("{action}-{target}").as_str());
    let filename = format!("{ts}_{slug}.md");
    let filepath = requests_dir.join(filename);

    let body = format!(
        "---\ntype: {action}\ntarget: {target}\nreason: {reason}\ncreated: {ts_full}\n---\n\n{content}\n",
        ts_full = ts.replace('T', " "),
        content = if content.is_empty() {
            "(no content provided)"
        } else {
            content
        }
    );

    std::fs::write(&filepath, body)?;
    Ok(filepath)
}

pub fn list_requests(requests_dir: &Path) -> Result<Vec<RequestMeta>> {
    if !requests_dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(requests_dir)? {
        let path = entry?.path();
        if path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Some(meta) = meta_from_path(&path) {
                entries.push(meta);
            }
        }
    }
    entries.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    Ok(entries)
}

pub fn get_request(id: &str, requests_dir: &Path) -> Result<String> {
    let path = resolve_request_file(id, requests_dir)?;
    std::fs::read_to_string(&path).with_context(|| format!("cannot read: {}", path.display()))
}

pub fn delete_request(id: &str, requests_dir: &Path) -> Result<()> {
    let path = resolve_request_file(id, requests_dir)?;
    std::fs::remove_file(&path).with_context(|| format!("cannot remove: {}", path.display()))?;
    Ok(())
}
