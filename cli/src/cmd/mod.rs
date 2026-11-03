//! Command dispatch: each `Commands` variant maps to one handler module.
//! Shared row/cron resolution lives here because task and cron both use it.

mod cli;
// сиблинг-тесты грамматики: без объявления cargo их молча не подхватывает
#[cfg(test)]
mod cli_tests;
mod cron;
mod docs;
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
        Commands::Search {
            query,
            limit,
            sofa,
            threshold,
        } => {
            search::run(
                &cfg,
                search::SearchCmd {
                    query,
                    limit,
                    sofa,
                    threshold,
                },
            )
            .await
        }
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
/// Исход резолва крона на крон-досках. Важен для разрушительного `cron rm`:
/// сирота-путь допустим только при однозначном «не найдено» на ВСЕХ читаемых
/// досках (неоднозначность и read-сбои — жёсткая ошибка, не «не найдено»).
#[derive(Debug)]
pub(crate) enum CronResolveFail {
    NotFound { details: String, all_readable: bool },
    Other(anyhow::Error),
}

impl std::fmt::Display for CronResolveFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { details, .. } => write!(f, "не найден ({details})"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CronResolveFail {}

pub(crate) async fn resolve_cron_any_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board_titles: &[String],
    root: &std::path::Path,
    id: &str,
) -> Result<(affine_tasks::Board, affine_tasks::TaskRow), CronResolveFail> {
    let mut last_err = String::new();
    // read_failures считают только транспортные/протокольные сбои; отсутствующая
    // доска («Crons» в трэше, «Кроны <proj>» не создана) — норма воркспейса
    let mut read_failures = 0usize;
    for board_title in board_titles {
        let board = match affine_tasks::find_board_typed(client, cfg, board_title).await {
            affine_tasks::BoardAccess::Found(board) => board,
            affine_tasks::BoardAccess::Absent => {
                last_err = format!("{board_title}: доски нет");
                continue;
            }
            affine_tasks::BoardAccess::Failed(e) => {
                read_failures += 1;
                last_err = format!("{board_title}: доска не читается ({e})");
                continue;
            }
        };
        let rows = match affine_tasks::read_rows(client, cfg, &board, root).await {
            Ok(r) => r,
            Err(e) => {
                read_failures += 1;
                last_err = format!("{board_title}: строки не читаются ({e})");
                continue;
            }
        };
        match affine_tasks::classify_row(&rows, id) {
            affine_tasks::RowMatch::One(row) => return Ok((board, row.clone())),
            affine_tasks::RowMatch::Ambiguous { .. } => {
                return Err(CronResolveFail::Other(anyhow::anyhow!(
                    "«{id}» неоднозначно на доске «{board_title}» — уточни id"
                )));
            }
            affine_tasks::RowMatch::NotFound => {
                last_err = format!("{board_title}: нет");
            }
        }
    }
    Err(CronResolveFail::NotFound {
        details: last_err,
        all_readable: read_failures == 0,
    })
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
