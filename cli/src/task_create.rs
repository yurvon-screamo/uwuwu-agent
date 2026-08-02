use std::path::Path;

use anyhow::{Context, Result};

use crate::enrich;
use crate::file_io;
use crate::path as safe_path;
use crate::time_util;

pub struct CreateArgs {
    pub project: String,
    pub task_key: String,
    pub content: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub priority: Option<String>,
    pub deadline: Option<String>,
    pub designed: bool,
}

pub async fn run_create(args: &CreateArgs, wiki_root: &Path) -> Result<()> {
    if args.content.trim().is_empty() {
        anyhow::bail!("content is empty");
    }

    let task_dir =
        safe_path::resolve_target_task_dir_for_create(&args.project, &args.task_key, wiki_root)?;
    std::fs::create_dir_all(&task_dir)
        .with_context(|| format!("cannot create dir: {}", task_dir.display()))?;

    let task_path = task_dir.join("README.md");

    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;

    let body = build_file_body(
        &args.content,
        args.title.as_deref(),
        args.description.as_deref(),
        &args.tags,
        args.priority.as_deref(),
        args.deadline.as_deref(),
        args.designed,
    );

    let prepared = file_io::normalize_line_endings(&body);
    file_io::atomic_write(&task_path, prepared.as_bytes(), &canonical_projects)?;

    if args.title.is_none() && args.description.is_none() {
        if let Err(e) = enrich::enrich_document(&task_path).await {
            eprintln!("warning: {e}");
        }
    }

    Ok(())
}

pub async fn handle_create(args: &CreateArgs, wiki_root: &Path) -> Result<()> {
    run_create(args, wiki_root).await?;
    println!("Task created: {}/{}", args.project, args.task_key);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_file_body(
    content: &str,
    title: Option<&str>,
    description: Option<&str>,
    tags: &[String],
    priority: Option<&str>,
    deadline: Option<&str>,
    designed: bool,
) -> String {
    let today = time_util::format_date_now();
    let priority_val = priority.unwrap_or("normal");
    let mut fm = format!(
        "created: {today}\nupdated: {today}\nstatus: open\npriority: {priority_val}\ndesigned: {designed}\n"
    );

    if let Some(dl) = deadline {
        fm.push_str(&format!("deadline: {dl}\n"));
    }

    if !tags.is_empty() {
        fm.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    }
    if let Some(t) = title {
        fm.push_str(&format!("title: {t}\n"));
    }
    if let Some(d) = description {
        fm.push_str(&format!("description: {d}\n"));
    }

    format!("---\n{fm}---\n\n{content}\n")
}
