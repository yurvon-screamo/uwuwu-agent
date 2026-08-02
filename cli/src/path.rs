use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

pub const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn validate_slug(slug: &str) -> Result<()> {
    if !slug_shape_valid(slug) {
        anyhow::bail!(
            "invalid slug '{slug}': must start with alphanumeric, contain only alphanumeric/dot/dash/underscore"
        );
    }
    if slug.ends_with('.') {
        anyhow::bail!("invalid slug '{slug}': must not end with dot");
    }
    let upper = slug.to_uppercase();
    if RESERVED_NAMES.contains(&upper.as_str()) {
        anyhow::bail!("invalid slug '{slug}': reserved Windows name");
    }
    Ok(())
}

fn slug_shape_valid(slug: &str) -> bool {
    let mut chars = slug.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub fn resolve_existing(
    project: &str,
    subdir: &str,
    slug: &str,
    wiki_root: &Path,
) -> Result<PathBuf> {
    let sanitized_project = sanitize_project(project)?;
    validate_slug(slug)?;

    let project_root = wiki_root.join("projects").join(&sanitized_project);
    let file_path = project_root.join(subdir).join(format!("{slug}.md"));

    if !file_path.exists() {
        anyhow::bail!("file not found: projects/{sanitized_project}/{subdir}/{slug}.md");
    }

    let canonical_file = file_path
        .canonicalize()
        .with_context(|| format!("cannot canonicalize: {}", file_path.display()))?;
    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;

    if !canonical_file.starts_with(&canonical_projects) {
        anyhow::bail!("path escapes projects/: projects/{sanitized_project}/{subdir}/{slug}.md");
    }

    Ok(canonical_file)
}

pub fn resolve_target_for_create(
    project: &str,
    subdir: &str,
    slug: &str,
    wiki_root: &Path,
) -> Result<PathBuf> {
    let sanitized_project = sanitize_project(project)?;
    validate_slug(slug)?;

    let project_root = wiki_root.join("projects").join(&sanitized_project);
    let subdir_path = project_root.join(subdir);
    let file_path = subdir_path.join(format!("{slug}.md"));

    if file_path.exists() {
        anyhow::bail!("file already exists: {}", file_path.display());
    }

    Ok(file_path)
}

pub fn project_subdir_doc_type(project: &str, subdir: &str) -> Result<String> {
    let sanitized = sanitize_project(project)?;
    Ok(format!("projects/{sanitized}/{subdir}"))
}

pub fn resolve_within_root(relpath: &str, root: &Path) -> Result<PathBuf> {
    let parsed = Path::new(relpath);
    if parsed.is_absolute() {
        anyhow::bail!("path must be root-relative, got absolute: {relpath}");
    }
    for component in parsed.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => anyhow::bail!("path must not contain '.': {relpath}"),
            Component::ParentDir => anyhow::bail!("path must not contain '..': {relpath}"),
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("path must be relative (no root/prefix): {relpath}")
            }
        }
    }

    let target = root.join(relpath);
    if !target.exists() {
        anyhow::bail!("not found: {}", target.display());
    }

    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("cannot canonicalize root: {}", root.display()))?;
    let canonical_target = target
        .canonicalize()
        .with_context(|| format!("cannot canonicalize: {}", target.display()))?;
    if !canonical_target.starts_with(&canonical_root) {
        anyhow::bail!("path escapes root: {relpath}");
    }

    Ok(canonical_target)
}

pub fn slug_from_relpath(relpath: &str) -> String {
    let path = Path::new(relpath);
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    if file_name == "README.md" || file_name == "README" {
        if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
            return parent.to_string_lossy().to_string();
        }
    }

    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| relpath.to_string())
}

pub fn resolve_task_dir(project: &str, slug: &str, wiki_root: &Path) -> Result<PathBuf> {
    let sanitized_project = sanitize_project(project)?;
    validate_slug(slug)?;

    let project_root = wiki_root.join("projects").join(&sanitized_project);
    let task_dir = project_root.join("tasks").join(slug);

    if !task_dir.exists() {
        anyhow::bail!("task not found: projects/{sanitized_project}/tasks/{slug}/");
    }

    let canonical_task_dir = task_dir
        .canonicalize()
        .with_context(|| format!("cannot canonicalize: {}", task_dir.display()))?;
    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;

    if !canonical_task_dir.starts_with(&canonical_projects) {
        anyhow::bail!("path escapes projects/: projects/{sanitized_project}/tasks/{slug}/");
    }

    Ok(canonical_task_dir)
}

pub fn resolve_task_readme(project: &str, slug: &str, wiki_root: &Path) -> Result<PathBuf> {
    let canonical_task_dir = resolve_task_dir(project, slug, wiki_root)?;
    let readme = canonical_task_dir.join("README.md");

    if !readme.exists() {
        if let Some(legacy_md) = canonical_task_dir
            .parent()
            .map(|p| p.join(format!("{slug}.md")))
        {
            if legacy_md.exists() {
                anyhow::bail!(
                    "legacy flat task file found: projects/{project}/tasks/{slug}.md — convert to folder structure (tasks/{slug}/README.md) manually"
                );
            }
        }
        anyhow::bail!("README.md not found in task dir: projects/{project}/tasks/{slug}/");
    }

    Ok(readme)
}

pub fn resolve_target_task_dir_for_create(
    project: &str,
    slug: &str,
    wiki_root: &Path,
) -> Result<PathBuf> {
    let sanitized_project = sanitize_project(project)?;
    validate_slug(slug)?;

    let project_root = wiki_root.join("projects").join(&sanitized_project);
    let tasks_dir = project_root.join("tasks");
    let task_dir = tasks_dir.join(slug);

    if !tasks_dir.exists() {
        anyhow::bail!("project tasks directory not found: projects/{sanitized_project}/tasks/");
    }

    if task_dir.exists() {
        anyhow::bail!("task already exists: projects/{sanitized_project}/tasks/{slug}/");
    }

    Ok(task_dir)
}

pub fn sanitize_project(project: &str) -> Result<String> {
    if project.is_empty() || project == "." || project == ".." {
        anyhow::bail!("invalid project name: '{project}'");
    }

    if project.contains('/') || project.contains('\\') || project.contains(':') {
        anyhow::bail!("invalid project name: '{project}' — must be a single path component");
    }

    let parsed = Path::new(project);
    if parsed.is_absolute() || parsed.components().count() != 1 {
        anyhow::bail!("invalid project name: '{project}' — must be a single path component");
    }

    if project.ends_with('.') {
        anyhow::bail!("invalid project name: '{project}' — must not end with dot");
    }

    let upper = project.to_uppercase();
    if RESERVED_NAMES.contains(&upper.as_str()) {
        anyhow::bail!("invalid project name: '{project}' — reserved Windows name");
    }

    Ok(project.to_string())
}
