use std::path::Path;

use anyhow::{Context, Result};

use crate::chunk;
use crate::file_io::{self, EditKind, EditOutcome};
use crate::path as safe_path;
use crate::time_util;

pub fn write_task_content(
    project: &str,
    slug: &str,
    content: &str,
    wiki_root: &Path,
) -> Result<EditOutcome> {
    if content.trim().is_empty() {
        anyhow::bail!("content is empty");
    }

    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;

    let task_dir = match safe_path::resolve_task_dir(project, slug, wiki_root) {
        Ok(dir) => dir,
        Err(_) => {
            safe_path::validate_slug(slug)?;
            let sanitized_project = safe_path::sanitize_project(project)?;
            let tasks_root = wiki_root
                .join("projects")
                .join(&sanitized_project)
                .join("tasks");
            if !tasks_root.exists() {
                anyhow::bail!("project tasks directory not found: projects/{project}/tasks/");
            }
            let new_dir = tasks_root.join(slug);
            if !new_dir.exists() {
                std::fs::create_dir_all(&new_dir)
                    .with_context(|| format!("cannot create dir: {}", new_dir.display()))?;
            }
            new_dir
                .canonicalize()
                .with_context(|| format!("cannot canonicalize: {}", new_dir.display()))?
        }
    };

    let target = task_dir.join("README.md");
    let kind = if target.exists() {
        EditKind::Updated
    } else {
        EditKind::Created
    };

    let today = time_util::format_date_now();
    let with_updated = chunk::update_frontmatter_fields(content, &[("updated", &today)]);
    let prepared = file_io::normalize_line_endings(&with_updated);

    file_io::atomic_write(&target, prepared.as_bytes(), &canonical_projects)?;

    Ok(EditOutcome { kind, path: target })
}

pub fn update_task_meta(
    project: &str,
    slug: &str,
    priority: Option<&str>,
    deadline: Option<&str>,
    clear_deadline: bool,
    designed: Option<bool>,
    wiki_root: &Path,
) -> Result<()> {
    let readme = safe_path::resolve_task_readme(project, slug, wiki_root)?;
    let content =
        std::fs::read_to_string(&readme).with_context(|| format!("cannot read: {readme:?}"))?;

    let (fm, _) = chunk::split_frontmatter(&content);
    if fm.trim().is_empty() {
        anyhow::bail!("not a task file (no valid frontmatter)");
    }

    let today = time_util::format_date_now();
    let mut updates: Vec<(&str, String)> = Vec::new();
    let mut removes: Vec<&str> = Vec::new();

    if let Some(p) = priority {
        updates.push(("priority", p.to_string()));
    }
    if let Some(dl) = deadline {
        updates.push(("deadline", dl.to_string()));
    }
    if clear_deadline {
        removes.push("deadline");
    }
    if let Some(d) = designed {
        updates.push(("designed", d.to_string()));
    }
    updates.push(("updated", today));

    let update_refs: Vec<(&str, &str)> = updates.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut new_content = chunk::update_frontmatter_fields(&content, &update_refs);

    if !removes.is_empty() {
        new_content = chunk::remove_frontmatter_fields(&new_content, &removes);
    }

    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;
    let prepared = file_io::normalize_line_endings(&new_content);
    file_io::atomic_write(&readme, prepared.as_bytes(), &canonical_projects)?;

    Ok(())
}

pub fn handle_edit(project: &str, slug: &str, content: &str, wiki_root: &Path) -> Result<()> {
    let outcome = write_task_content(project, slug, content, wiki_root)?;
    let kind = match outcome.kind {
        EditKind::Created => "Task created",
        EditKind::Updated => "Task updated",
    };
    println!("{kind}: {project}/{slug}");
    Ok(())
}
