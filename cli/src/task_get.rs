use std::path::Path;

use anyhow::Result;

use crate::path as safe_path;

pub fn handle_get(project: &str, slug: &str, wiki_root: &Path) -> Result<()> {
    let canonical = safe_path::resolve_task_readme(project, slug, wiki_root)?;
    let content = std::fs::read_to_string(&canonical)?;
    println!("{content}");
    Ok(())
}
