//! FS collectors for the AFFiNE migration: pure functions reading the wiki
//! root into migration units. Unreadable/non-UTF8 files surface as errors
//! instead of migrating as empty documents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::affine_store::{split_frontmatter, DocToMigrate, FactEntry};

/// Collected documents plus read errors: a failed read must NOT mark the
/// slug as migrated (the map only records successful pushes).
pub struct CollectResult<T> {
    pub items: Vec<T>,
    pub errors: Vec<String>,
}

fn collect_md(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_md(dir, &mut out);
    out.sort();
    out
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_md(&path, out);
        } else if path.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn collect_experience(root: &Path) -> CollectResult<DocToMigrate> {
    let base = root.join("experience");
    let paths = collect_md(&base);
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for path in &paths {
        let content = match read_file(path) {
            Ok(content) => content,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let rel = path.strip_prefix(&base).unwrap_or(path);
        let slug = rel.with_extension("").to_string_lossy().replace('\\', "/");
        let (fm, body) = split_frontmatter(&content);
        items.push(DocToMigrate {
            title: fm.title.unwrap_or_else(|| slug.clone()),
            markdown: body,
            slug,
        });
    }
    CollectResult { items, errors }
}

pub struct ProjectDocs {
    pub name: String,
    pub readme: String,
    pub docs: Vec<DocToMigrate>,
}

pub fn collect_projects(root: &Path) -> CollectResult<ProjectDocs> {
    let projects_root = root.join("projects");
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return CollectResult { items, errors };
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let readme = match std::fs::read_to_string(dir.join("README.md")) {
            Ok(raw) => split_frontmatter(&raw).1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("# {name}"),
            Err(e) => {
                errors.push(format!("{}README.md: {e}", dir.display()));
                format!("# {name}")
            }
        };
        let paths = collect_md(&dir);
        let docs = paths
            .iter()
            .filter(|p| p.file_name().map(|n| n != "README.md").unwrap_or(false))
            .filter_map(|path| {
                let content = read_file(path).ok()?;
                let slug = format!(
                    "{}/{}",
                    name,
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                let (fm, body) = split_frontmatter(&content);
                Some(DocToMigrate {
                    title: fm.title.unwrap_or_else(|| slug.clone()),
                    markdown: body,
                    slug,
                })
            })
            .collect();
        items.push(ProjectDocs { name, readme, docs });
    }
    CollectResult { items, errors }
}

pub fn collect_facts(root: &Path) -> CollectResult<FactEntry> {
    let paths = collect_md(&root.join("facts"));
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for path in &paths {
        let content = match read_file(path) {
            Ok(content) => content,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let slug = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let (fm, statement) = split_frontmatter(&content);
        items.push(FactEntry {
            date: fm.updated.clone().unwrap_or_else(|| month_fallback(path)),
            title: fm.title.unwrap_or_else(|| slug.clone()),
            statement,
            tags: fm.tags,
            confidence: fm.confidence.unwrap_or_else(|| "-".into()),
            sources: fm.sources,
            slug,
        });
    }
    CollectResult { items, errors }
}

fn month_fallback(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| format!("{}-01", n.to_string_lossy()))
        .unwrap_or_else(|| "1970-01-01".into())
}

pub fn group_by_date(facts: &[FactEntry]) -> BTreeMap<String, Vec<&FactEntry>> {
    let mut groups: BTreeMap<String, Vec<&FactEntry>> = BTreeMap::new();
    for fact in facts {
        groups.entry(fact.date.clone()).or_default().push(fact);
    }
    groups
}
