//! Command dispatch: each `Commands` variant maps to one handler module.
//! Shared row/cron resolution lives here because task and cron both use it.

mod cli;
mod cron;
mod docs;
mod facts;
mod request;
mod search;
mod task;

use clap::Parser as _;

pub(crate) use cli::{Cli, Commands, CronSub, TaskSub};

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_tasks;

pub(crate) async fn run() -> anyhow::Result<()> {
    crate::init_tracing();
    let cli = Cli::parse();
    let root = crate::wiki_root();
    let cfg = AffineConfig::from_env()?;

    match cli.command {
        Commands::Read { slug } => docs::read(&cfg, &root, slug).await,
        Commands::Blob { key, out } => docs::blob(&cfg, &key, out).await,
        Commands::Shot {
            slug,
            out,
            edgeless,
        } => docs::shot(&cfg, &root, slug, out, edgeless).await,
        Commands::Projects => docs::projects(&cfg, &root).await,
        Commands::Refresh => docs::refresh(&cfg, &root).await,
        Commands::Facts {
            stdin,
            source,
            date,
            text,
        } => facts::run(stdin, source, date, text).await,
        Commands::Request {
            command,
            text,
            stdin,
            action,
            target,
            reason,
            attach,
        } => {
            request::run(
                &cfg,
                &root,
                request::RequestCmd {
                    command,
                    text,
                    stdin,
                    action,
                    target,
                    reason,
                    attach,
                },
            )
            .await
        }
        Commands::Search {
            query,
            top,
            tasks,
            facts,
            access,
            sofa,
            threshold,
        } => {
            search::run(
                &cfg,
                &root,
                search::SearchCmd {
                    query,
                    top,
                    tasks,
                    facts,
                    access,
                    sofa,
                    threshold,
                },
            )
            .await
        }
        Commands::Grep { pattern } => search::grep(&cfg, &root, pattern).await,
        Commands::Cron { command } => cron::run(&cfg, &root, command).await,
        Commands::Task { command, project } => task::run(&cfg, &root, project, command).await,
    }
}

/// Резолв строки по id/заголовку: сперва доска проекта, затем доска Crons
/// (крон-агенты комментят свои тела через тот же task comment).
pub(crate) async fn resolve_row_any_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: &str,
) -> anyhow::Result<(affine_tasks::Board, affine_tasks::TaskRow)> {
    let board = affine_tasks::find_board(client, cfg, project).await?;
    let rows = affine_tasks::read_rows(client, cfg, &board, root).await?;
    if let Ok(row) = affine_tasks::resolve_row(&rows, id) {
        return Ok((board, row.clone()));
    }
    // фоллбек: крон-доски (Crons + «Кроны <proj>» всех проектов)
    for bt in affine_tasks::cron_board_titles(client, cfg).await? {
        if bt == "Crons" {
            continue;
        }
        let Ok(board) = affine_tasks::find_board_by_title(client, cfg, &bt).await else {
            continue;
        };
        let Ok(rows) = affine_tasks::read_rows(client, cfg, &board, root).await else {
            continue;
        };
        if let Ok(row) = affine_tasks::resolve_row(&rows, id) {
            return Ok((board, row.clone()));
        }
    }
    let cron_board = affine_tasks::find_board_by_title(client, cfg, "Crons").await?;
    let cron_rows = affine_tasks::read_rows(client, cfg, &cron_board, root).await?;
    let row = affine_tasks::resolve_row(&cron_rows, id)
        .map_err(|e| anyhow::anyhow!("{e} (искали на доске проекта и всех крон-досках)"))?;
    Ok((cron_board, row.clone()))
}

/// Найти крон на любой крон-доске (Crons + «Кроны <proj>» проектов).
pub(crate) async fn resolve_cron_any_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board_titles: &[String],
    root: &std::path::Path,
    id: &str,
) -> anyhow::Result<(affine_tasks::Board, affine_tasks::TaskRow)> {
    let mut last_err = String::new();
    for board_title in board_titles {
        let Ok(board) = affine_tasks::find_board_by_title(client, cfg, board_title).await else {
            continue;
        };
        let Ok(rows) = affine_tasks::read_rows(client, cfg, &board, root).await else {
            continue;
        };
        match affine_tasks::resolve_row(&rows, id) {
            Ok(row) => return Ok((board, row.clone())),
            Err(e) => last_err = format!("{e}"),
        }
    }
    anyhow::bail!("крон «{id}» не найден на крон-досках ({last_err})")
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
