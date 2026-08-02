use std::path::Path;

use anyhow::{Context, Result};

use crate::chunk;
use crate::file_io;
use crate::path as safe_path;
use crate::time_util;

pub fn apply_set_status(
    project: &str,
    slug: &str,
    new_status: &str,
    wiki_root: &Path,
) -> Result<()> {
    let canonical = safe_path::resolve_task_readme(project, slug, wiki_root)?;
    let content = std::fs::read_to_string(&canonical).context("cannot read task file")?;

    let (fm, _) = chunk::split_frontmatter(&content);
    if fm.trim().is_empty() {
        anyhow::bail!("not a task file (no valid frontmatter)");
    }

    let today = time_util::format_date_now();
    let updated =
        chunk::update_frontmatter_fields(&content, &[("status", new_status), ("updated", &today)]);

    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;
    let prepared = file_io::normalize_line_endings(&updated);
    file_io::atomic_write(&canonical, prepared.as_bytes(), &canonical_projects)?;

    Ok(())
}

pub fn handle_set_status(
    project: &str,
    slug: &str,
    new_status: &str,
    wiki_root: &Path,
) -> Result<()> {
    apply_set_status(project, slug, new_status, wiki_root)?;
    println!("Status updated: {project}/{slug} → {new_status}");
    Ok(())
}
