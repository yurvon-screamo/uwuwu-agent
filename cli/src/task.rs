use std::path::Path;

use anyhow::Result;
use clap::Subcommand;

use crate::task_create::{self, CreateArgs};
use crate::task_edit;
use crate::task_get;
use crate::task_grep;
use crate::task_list::{self, ListArgs};
use crate::task_search::{self, parse_date, SearchArgs};
use crate::task_set_status;
use crate::worklog;

pub const STATUS_VALUES: [&str; 4] = ["open", "in_progress", "blocked", "closed"];
pub const PRIORITY_VALUES_TASK: [&str; 3] = ["low", "normal", "high"];

#[derive(Subcommand)]
pub enum TaskSub {
    /// Semantic search across project tasks with optional filters.
    Search {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Search query")]
        query: String,
        #[arg(
            short,
            long,
            default_value = "3",
            help = "Number of top results to return"
        )]
        top: usize,
        #[arg(long, value_parser = STATUS_VALUES, help = "Filter by status: open | in_progress | blocked | closed")]
        status: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Filter by created date (inclusive lower bound): YYYY-MM-DD")]
        from: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Filter by created date (inclusive upper bound): YYYY-MM-DD")]
        to: Option<String>,
        #[arg(long, value_parser = PRIORITY_VALUES_TASK, help = "Filter by priority: low | normal | high")]
        priority: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive lower bound on deadline: YYYY-MM-DD")]
        deadline_from: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive upper bound on deadline: YYYY-MM-DD")]
        deadline_to: Option<String>,
        #[arg(long, help = "Filter by designed flag (true/false)")]
        designed: Option<bool>,
    },
    /// List project tasks filtered by status (no ranking, sorted by priority → deadline → created).
    List {
        #[arg(
            help = "Project name (optional; if omitted, lists across all projects grouped by project)"
        )]
        project: Option<String>,
        #[arg(long, value_parser = STATUS_VALUES, help = "Filter by status: open | in_progress | blocked | closed. If omitted, all tasks except closed.")]
        status: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive lower bound: YYYY-MM-DD")]
        from: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive upper bound: YYYY-MM-DD")]
        to: Option<String>,
        #[arg(long, value_parser = PRIORITY_VALUES_TASK, help = "Filter by priority: low | normal | high")]
        priority: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive lower bound on deadline: YYYY-MM-DD")]
        deadline_from: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Inclusive upper bound on deadline: YYYY-MM-DD")]
        deadline_to: Option<String>,
        #[arg(long, help = "Filter by designed flag (true/false)")]
        designed: Option<bool>,
    },
    /// Substring search across project tasks.
    Grep {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Search pattern (literal substring)")]
        pattern: String,
    },
    /// Read a task by project + slug (full content: frontmatter + body + worklogs).
    Get {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
    },
    /// Append a worklog entry to a task (timestamp auto, frontmatter untouched).
    Worklog {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
        #[arg(help = "Worklog text")]
        text: String,
    },
    /// Change task status; updates `updated` field in frontmatter.
    SetStatus {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
        #[arg(value_parser = STATUS_VALUES, help = "New status: open | in_progress | blocked | closed")]
        status: String,
    },
    /// Create or overwrite a task's full content (frontmatter + body). `updated` is auto-stamped.
    /// Or do a meta-only update with `--priority`/`--deadline`/`--designed` (cannot be combined with `--content`).
    Edit {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Full task content (frontmatter + body). Mutually exclusive with meta-flags."
        )]
        content: Option<String>,
        #[arg(long, value_parser = PRIORITY_VALUES_TASK, help = "Set priority: low | normal | high")]
        priority: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Set deadline (YYYY-MM-DD)")]
        deadline: Option<String>,
        #[arg(long, help = "Clear deadline field")]
        clear_deadline: bool,
        #[arg(long, help = "Mark task as designed (architectural work done)")]
        designed: bool,
        #[arg(long, help = "Mark task as NOT designed")]
        no_designed: bool,
    },
    /// Create a new task with frontmatter (open status by default).
    Create {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task folder name (will create tasks/<task_key>/README.md)")]
        task_key: String,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Task body content (markdown)"
        )]
        content: String,
        #[arg(long, help = "Task title (EN, frontmatter)")]
        title: Option<String>,
        #[arg(long, help = "Task description (EN, frontmatter)")]
        description: Option<String>,
        #[arg(long, help = "Tag (repeatable)", num_args = 0..)]
        tag: Vec<String>,
        #[arg(long, value_parser = PRIORITY_VALUES_TASK, help = "Task priority: low | normal | high (default: normal)")]
        priority: Option<String>,
        #[arg(long, value_parser = parse_date, help = "Task deadline (YYYY-MM-DD)")]
        deadline: Option<String>,
        #[arg(long, help = "Mark task as designed (architectural work done)")]
        designed: bool,
    },
    /// Clone a task with its assets into a local working directory (default: ./.uwuwu-workspace/tasks/<slug>/).
    Clone {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
        #[arg(
            long,
            help = "Custom destination (cwd-relative), overrides default .uwuwu-workspace/tasks/<slug>/"
        )]
        dest: Option<String>,
        #[arg(long, help = "Overwrite existing destination")]
        force: bool,
    },
    /// Read a task asset (text or image). Use for inspecting images and text assets attached to a task.
    AssetGet {
        #[arg(help = "Project name")]
        project: String,
        #[arg(help = "Task slug (folder name)")]
        slug: String,
        #[arg(help = "Asset filename (relative to task folder)")]
        asset: String,
    },
}

pub async fn handle(command: TaskSub, wiki_root: &Path, cache_path: &Path) -> Result<()> {
    match command {
        TaskSub::Search {
            project,
            query,
            top,
            status,
            from,
            to,
            priority,
            deadline_from,
            deadline_to,
            designed,
        } => {
            task_search::handle_search(
                &SearchArgs {
                    project,
                    query,
                    top,
                    status,
                    from,
                    to,
                    priority,
                    deadline_from,
                    deadline_to,
                    designed,
                },
                wiki_root,
                cache_path,
            )
            .await
        }
        TaskSub::List {
            project,
            status,
            from,
            to,
            priority,
            deadline_from,
            deadline_to,
            designed,
        } => task_list::handle_list(
            &ListArgs {
                project,
                status,
                from,
                to,
                priority,
                deadline_from,
                deadline_to,
                designed,
            },
            wiki_root,
        ),
        TaskSub::Grep { project, pattern } => task_grep::handle_grep(&project, &pattern, wiki_root),
        TaskSub::Get { project, slug } => task_get::handle_get(&project, &slug, wiki_root),
        TaskSub::Worklog {
            project,
            slug,
            text,
        } => worklog::append(&project, &slug, &text, wiki_root),
        TaskSub::SetStatus {
            project,
            slug,
            status,
        } => task_set_status::handle_set_status(&project, &slug, &status, wiki_root),
        TaskSub::Edit {
            project,
            slug,
            content,
            priority,
            deadline,
            clear_deadline,
            designed,
            no_designed,
        } => {
            let has_meta = priority.is_some()
                || deadline.is_some()
                || clear_deadline
                || designed
                || no_designed;

            match (content, has_meta) {
                (Some(_), true) => anyhow::bail!(
                    "cannot mix --content with meta flags (--priority/--deadline/--clear-deadline/--designed/--no-designed)"
                ),
                (None, false) => anyhow::bail!(
                    "must provide either --content or at least one meta-flag (--priority/--deadline/--clear-deadline/--designed/--no-designed)"
                ),
                (Some(c), false) => task_edit::handle_edit(&project, &slug, &c, wiki_root),
                (None, true) => {
                    if designed && no_designed {
                        anyhow::bail!("cannot pass both --designed and --no-designed");
                    }
                    let designed_val = if designed {
                        Some(true)
                    } else if no_designed {
                        Some(false)
                    } else {
                        None
                    };
                    task_edit::update_task_meta(
                        &project,
                        &slug,
                        priority.as_deref(),
                        deadline.as_deref(),
                        clear_deadline,
                        designed_val,
                        wiki_root,
                    )?;
                    println!("Task meta updated: {project}/{slug}");
                    Ok(())
                }
            }
        }
        TaskSub::Create {
            project,
            task_key,
            content,
            title,
            description,
            tag,
            priority,
            deadline,
            designed,
        } => {
            task_create::handle_create(
                &CreateArgs {
                    project,
                    task_key,
                    content,
                    title,
                    description,
                    tags: tag,
                    priority,
                    deadline,
                    designed,
                },
                wiki_root,
            )
            .await
        }
        TaskSub::Clone {
            project,
            slug,
            dest,
            force,
        } => crate::task_clone::handle_clone(&project, &slug, dest.as_deref(), force, wiki_root),
        TaskSub::AssetGet {
            project,
            slug,
            asset,
        } => crate::task_asset::handle_asset_get(&project, &slug, &asset, wiki_root),
    }
}
