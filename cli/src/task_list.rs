use std::path::Path;

use anyhow::Result;

use crate::chunk;
use crate::path as safe_path;
use crate::task_list_render;
use crate::task_query::{deadline_sort_key, priority_rank, TaskQuery};

pub struct ListArgs {
    pub project: Option<String>,
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub priority: Option<String>,
    pub deadline_from: Option<String>,
    pub deadline_to: Option<String>,
    pub designed: Option<bool>,
}

pub struct ListEntry {
    pub slug: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub created: Option<String>,
    pub priority: Option<String>,
    pub deadline: Option<String>,
    pub designed: Option<bool>,
}

pub fn collect_groups(args: &ListArgs, wiki_root: &Path) -> Result<Vec<(String, Vec<ListEntry>)>> {
    let filter = TaskQuery {
        status: args.status.clone(),
        from: args.from.clone(),
        to: args.to.clone(),
        priority: args.priority.clone(),
        deadline_from: args.deadline_from.clone(),
        deadline_to: args.deadline_to.clone(),
        designed: args.designed,
    };

    match &args.project {
        Some(p) => {
            let sanitized = safe_path::sanitize_project(p)?;
            let dir = wiki_root.join("projects").join(&sanitized).join("tasks");
            if !dir.exists() {
                return Ok(vec![(sanitized, vec![])]);
            }
            Ok(vec![(sanitized, collect_matching(&dir, &filter))])
        }
        None => {
            let projects_root = wiki_root.join("projects");
            Ok(collect_all_projects(&projects_root, &filter))
        }
    }
}

pub fn handle_list(args: &ListArgs, wiki_root: &Path) -> Result<()> {
    let is_multi = args.project.is_none();

    match &args.project {
        Some(p) => {
            let sanitized = safe_path::sanitize_project(p)?;
            let dir = wiki_root.join("projects").join(&sanitized).join("tasks");
            if !dir.exists() {
                println!("No tasks in project '{sanitized}' (tasks folder does not exist).");
                return Ok(());
            }
        }
        None => {
            let projects_root = wiki_root.join("projects");
            if !projects_root.exists() {
                println!("No projects found (projects/ directory does not exist).");
                return Ok(());
            }
        }
    }

    let groups = collect_groups(args, wiki_root)?;
    let non_empty: Vec<_> = groups.into_iter().filter(|(_, e)| !e.is_empty()).collect();

    if non_empty.is_empty() {
        match &args.status {
            Some(s) => println!("No tasks with status={s}."),
            None => println!("No non-closed tasks found."),
        }
        return Ok(());
    }

    for (project, entries) in &non_empty {
        if is_multi {
            println!("\nProject: {project}");
        }
        task_list_render::render_task_table(entries);
    }
    Ok(())
}

fn collect_matching(project_tasks_dir: &Path, filter: &TaskQuery) -> Vec<ListEntry> {
    let mut files = Vec::new();
    collect_md_recursive(project_tasks_dir, &mut files);

    let mut entries: Vec<ListEntry> = files
        .into_iter()
        .filter(|p| filter.matches_default_exclude_closed(p))
        .map(|p| {
            let (title, description) = chunk::read_meta(&p);
            let meta = chunk::read_task_meta(&p);
            let slug = p
                .parent()
                .and_then(|d| d.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            ListEntry {
                slug,
                title,
                description,
                created: meta.created,
                priority: meta.priority,
                deadline: meta.deadline,
                designed: meta.designed,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        priority_rank(a.priority.as_deref())
            .cmp(&priority_rank(b.priority.as_deref()))
            .then_with(|| deadline_sort_key(&a.deadline).cmp(&deadline_sort_key(&b.deadline)))
            .then(a.created.cmp(&b.created))
            .then(a.slug.cmp(&b.slug))
    });
    entries
}

fn collect_all_projects(projects_root: &Path, filter: &TaskQuery) -> Vec<(String, Vec<ListEntry>)> {
    let mut groups = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_root) else {
        return groups;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let tasks_dir = path.join("tasks");
        if !tasks_dir.exists() {
            continue;
        }
        let entries = collect_matching(&tasks_dir, filter);
        groups.push((name, entries));
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

fn collect_md_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("README.md").exists() {
                    files.push(path.join("README.md"));
                }
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                let file_name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if file_name != "README.md" && file_name != ".gitkeep" {
                    files.push(path);
                }
            }
        }
    }
}
