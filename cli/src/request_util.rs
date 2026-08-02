use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::path as safe_path;
use crate::time_util;

pub struct RequestMeta {
    pub id: String,
    pub action: String,
    pub target: String,
    pub reason: String,
    pub created: String,
}

pub fn meta_from_path(path: &Path) -> Option<RequestMeta> {
    let content = std::fs::read_to_string(path).ok()?;
    let (fm, _) = crate::chunk::split_frontmatter(&content);
    let fields = crate::chunk::parse_frontmatter_fields(&fm).unwrap_or_default();
    let id = path.file_stem()?.to_string_lossy().to_string();
    Some(RequestMeta {
        id,
        action: fields.get("type").cloned().unwrap_or_default(),
        target: fields.get("target").cloned().unwrap_or_default(),
        reason: fields.get("reason").cloned().unwrap_or_default(),
        created: fields.get("created").cloned().unwrap_or_default(),
    })
}

pub fn resolve_request_file(id: &str, requests_dir: &Path) -> Result<PathBuf> {
    safe_path::validate_slug(id)?;

    let exact = requests_dir.join(format!("{id}.md"));
    if exact.is_file() {
        return contained(exact, requests_dir);
    }

    let mut matches = Vec::new();
    if requests_dir.exists() {
        for entry in std::fs::read_dir(requests_dir)?.flatten() {
            let path = entry.path();
            let stem_matches = path
                .file_stem()
                .map(|s| s.to_string_lossy().starts_with(id))
                .unwrap_or(false);
            if stem_matches && path.extension().map(|e| e == "md").unwrap_or(false) {
                matches.push(path);
            }
        }
    }
    match matches.len() {
        0 => bail!("request not found: {id}"),
        1 => contained(matches.remove(0), requests_dir),
        _ => bail!("ambiguous request id '{id}': {} matches", matches.len()),
    }
}

fn contained(path: PathBuf, requests_dir: &Path) -> Result<PathBuf> {
    let canonical_file = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalize: {}", path.display()))?;
    let canonical_dir = requests_dir
        .canonicalize()
        .with_context(|| "requests dir not found")?;
    if !canonical_file.starts_with(&canonical_dir) {
        bail!("path escapes requests dir: {}", path.display());
    }
    Ok(canonical_file)
}

pub fn compact_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs = now % 60;
    let mins = (now / 60) % 60;
    let hours = (now / 3600) % 24;
    let days = now / 86400;
    let (year, month, day) = time_util::days_to_ymd(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}{mins:02}{secs:02}")
}

pub fn slugify(text: &str) -> String {
    let slug: String = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    result.trim_matches('-').chars().take(80).collect()
}
