use std::path::Path;

use anyhow::Result;

use crate::grep;
use crate::path as safe_path;

pub fn handle_grep(project: &str, pattern: &str, wiki_root: &Path) -> Result<()> {
    let doc_type = safe_path::project_subdir_doc_type(project, "tasks")?;

    let matches = grep::grep(pattern, &doc_type, wiki_root)?;

    if matches.is_empty() {
        println!("  No matches found.");
        return Ok(());
    }

    let mut last_file = String::new();
    for m in &matches {
        let slug = safe_path::slug_from_relpath(&m.filepath);
        if slug != last_file {
            println!("\n{slug}");
            last_file = slug;
        }
        println!("  {}: {}", m.line_number, m.line);
    }
    println!(
        "\n{} matches in {} files",
        matches.len(),
        matches
            .iter()
            .map(|m| &m.filepath)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );

    Ok(())
}
