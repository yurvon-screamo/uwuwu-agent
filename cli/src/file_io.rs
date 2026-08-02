use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Created,
    Updated,
}

#[derive(Debug, Clone)]
pub struct EditOutcome {
    pub kind: EditKind,
    pub path: PathBuf,
}

pub(crate) fn collect_normal_components(relpath: &str) -> Result<Vec<String>> {
    let parsed = Path::new(relpath);

    if parsed.is_absolute() {
        bail!("relpath must be relative: {relpath}");
    }

    let mut out = Vec::new();
    for component in parsed.components() {
        match component {
            Component::Normal(os_str) => {
                let s = os_str
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-UTF8 path component: {relpath}"))?;
                validate_component(s)?;
                out.push(s.to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("relpath must be relative (no root/prefix): {relpath}");
            }
            Component::CurDir => {
                bail!("relpath must not contain '.': {relpath}");
            }
            Component::ParentDir => {
                bail!("relpath must not contain '..': {relpath}");
            }
        }
    }

    if out.is_empty() {
        bail!("relpath is empty: {relpath}");
    }
    Ok(out)
}

fn validate_component(s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("empty path component");
    }
    if s.starts_with('.') || s.ends_with('.') {
        bail!("path component must not start/end with dot: {s}");
    }
    for ch in s.chars() {
        let allowed = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.';
        if !allowed {
            bail!("path component contains forbidden char '{ch}': {s}");
        }
    }
    Ok(())
}

pub(crate) fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n")
}

pub(crate) fn atomic_write(target: &Path, bytes: &[u8], containment_root: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent"))?;

    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create dirs: {}", parent.display()))?;
    }

    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("cannot canonicalize parent: {}", parent.display()))?;
    if !canonical_parent.starts_with(containment_root) {
        bail!("target parent escapes containment root");
    }

    let temp = canonical_parent.join(format!(
        ".{}.tmp",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("uwuwu_edit")
    ));

    if let Err(e) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot write temp: {}", temp.display()));
    }

    if let Err(e) = std::fs::rename(&temp, target) {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot rename temp -> {}", target.display()));
    }

    Ok(())
}
