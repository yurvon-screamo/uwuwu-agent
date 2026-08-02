use std::path::{Path, PathBuf};

use anyhow::Result;

pub struct GrepMatch {
    pub filepath: String,
    pub line_number: usize,
    pub line: String,
}

pub fn grep(pattern: &str, doc_type: &str, wiki_root: &Path) -> Result<Vec<GrepMatch>> {
    let base_dir = wiki_root.join(doc_type);
    if !base_dir.exists() {
        eprintln!("Error: directory '{}' not found", base_dir.display());
        return Ok(vec![]);
    }

    let mut files = Vec::new();
    collect_md(&base_dir, &mut files);
    files.sort();

    let mut matches = Vec::new();

    for f in &files {
        let rel = f
            .strip_prefix(wiki_root)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (i, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                matches.push(GrepMatch {
                    filepath: rel.clone(),
                    line_number: i + 1,
                    line: line.to_string(),
                });
            }
        }
    }

    Ok(matches)
}

fn collect_md(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_md(&path, files);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                files.push(path);
            }
        }
    }
}
