use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

use crate::chunk;

pub struct ProjectEntry {
    pub name: String,
    pub access_count: usize,
    pub tasks_count: usize,
    pub description: String,
}

pub fn collect_projects(wiki_root: &Path) -> Result<Vec<ProjectEntry>> {
    let projects_root = wiki_root.join("projects");
    if !projects_root.exists() {
        return Ok(vec![]);
    }

    let mut entries: Vec<ProjectEntry> = Vec::new();
    for entry in std::fs::read_dir(&projects_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let access_count = count_md_files(&path.join("access"));
        let tasks_count = count_tasks(&path.join("tasks"));
        let readme = path.join("README.md");
        let description = if readme.exists() {
            let content = std::fs::read_to_string(&readme).unwrap_or_default();
            let (_, body) = chunk::split_frontmatter(&content);
            body.lines()
                .skip_while(|l| l.starts_with('#'))
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        entries.push(ProjectEntry {
            name,
            access_count,
            tasks_count,
            description,
        });
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub fn handle_projects(wiki_root: &Path) -> Result<()> {
    let projects_root = wiki_root.join("projects");
    if !projects_root.exists() {
        println!("No projects found (projects/ directory does not exist).");
        return Ok(());
    }

    let entries = collect_projects(wiki_root)?;
    if entries.is_empty() {
        println!("No projects found.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Project").add_attribute(Attribute::Bold),
            Cell::new("Access"),
            Cell::new("Tasks"),
            Cell::new("Description"),
        ]);

    if !std::io::stdout().is_terminal() {
        table.set_width(180);
    }

    for e in &entries {
        table.add_row(vec![
            Cell::new(&e.name).fg(Color::Cyan),
            Cell::new(e.access_count).fg(Color::DarkGrey),
            Cell::new(e.tasks_count).fg(Color::DarkGrey),
            Cell::new(&e.description),
        ]);
    }

    println!("{table}");
    Ok(())
}

fn count_md_files(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let mut count = 0;
    count_md_recursive(dir, &mut count);
    count
}

fn count_md_recursive(dir: &Path, count: &mut usize) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count_md_recursive(&path, count);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                *count += 1;
            }
        }
    }
}

fn count_tasks(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("README.md").exists() {
                    count += 1;
                }
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name != "README.md" && name != ".gitkeep" {
                    count += 1;
                }
            }
        }
    }
    count
}
