use std::path::{Path, PathBuf};

use anyhow::Result;

pub fn get_document_by_slug(slug: &str, wiki_root: &Path) -> Result<String> {
    let matches = resolve_by_slug(slug, &wiki_root.join("experience"))?;
    match matches.len() {
        0 => anyhow::bail!("article not found by slug: '{slug}'"),
        1 => {
            let content = std::fs::read_to_string(&matches[0])?;
            let (_, body) = split_frontmatter(&content);
            Ok(body)
        }
        _ => {
            let list = matches
                .iter()
                .map(|p| {
                    p.strip_prefix(wiki_root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("ambiguous slug '{slug}': {list}")
        }
    }
}

fn resolve_by_slug(slug: &str, base_dir: &Path) -> Result<Vec<PathBuf>> {
    let target = format!("{slug}.md");
    let mut found = Vec::new();
    collect_by_basename(base_dir, &target, &mut found);
    Ok(found)
}

fn collect_by_basename(dir: &Path, basename: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_by_basename(&path, basename, out);
        } else if path.file_name().map(|n| n == basename).unwrap_or(false) {
            out.push(path);
        }
    }
}

fn split_frontmatter(content: &str) -> (String, String) {
    if !content.starts_with("---") {
        return (String::new(), content.to_string());
    }
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return (String::new(), content.to_string());
    }
    (parts[1].to_string(), parts[2].to_string())
}
