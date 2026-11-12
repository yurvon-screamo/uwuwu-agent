//! Handlers всех команд uwuwu-cli на SiYuan-бекенде (единственный).
//!
//! Домены: task/ (task-семья), goal, person, cron (+cron_list, cells), search,
//! read, kernel, body (+sections, comments), guards (общие гварды входа), attach.

pub(crate) mod task;

mod attach;
mod body;
mod body_comments;
mod body_sections;
mod cells;
mod cron;
mod cron_list;
mod goal;
mod guards;
mod kernel;
mod person;
mod read;
mod search;
mod strip;

#[cfg(test)]
mod attach_tests;
#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod create_tests;
#[cfg(test)]
mod diagram_tests;
#[cfg(test)]
mod edit_title_tests;
#[cfg(test)]
mod goal_tests;
#[cfg(test)]
mod guards_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod task_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub use read::read_markdown;
pub use search::search_lines;

// Поверхность для тестов-сиблингов cmd/*_tests.rs: пути crate::cmd::sy_dispatch::… сохранены
// (существуют только в тестовой сборке — в прод-юните не юзаются)
#[cfg(test)]
pub(crate) use cron::{cron_run, format_local, last_run_from_board, parse_state, run_card_owner};
#[cfg(test)]
pub(crate) use cron_list::cron_list;
#[cfg(test)]
pub(crate) use goal::{format_goal_table, goal_run};
#[cfg(test)]
pub(crate) use read::ls_lines;
#[cfg(test)]
pub(crate) use search::{doc_section_lines, doc_section_result_lines, fulltext_lines};
#[cfg(test)]
pub(crate) use task::task_run;

use crate::cmd::cli::Commands;
use anyhow::anyhow;

pub(crate) async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Task { command, project } => {
            let project = project.unwrap_or_default();
            task::task_run(&project, command).await
        }
        Commands::Goal { command, project } => {
            let project = project.unwrap_or_default();
            goal::goal_run(&project, command).await
        }
        Commands::Cron { command } => cron::cron_run(command).await,
        Commands::Search { query, limit, sofa, threshold } => {
            search::search_run(&query, limit, threshold, sofa).await
        }
        Commands::Semantic { command } => search::semantic_run(command).await,
        Commands::Read { slug } => {
            let md = read::read_markdown(&slug).await?;
            print!("{md}");
            Ok(())
        }
        Commands::Docs { command } => crate::cmd::docs_ls::docs_run(command).await,
        Commands::Blob { key, out } => read::blob_run(&key, out).await,
        Commands::Person { command } => person::person_run(command).await,
        Commands::Projects => read::projects_run().await,
        Commands::Shot { .. } => Err(anyhow!(
            "shot: не портирован на SiYuan (v1) — headless chromium на http://127.0.0.1:6806 с Access Auth Code"
        )),
        Commands::Kernel { command, timeout } => kernel::kernel_run(command, timeout).await,
        Commands::Doctor { project, since } => read::doctor_run(&project, since).await,
    }
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
