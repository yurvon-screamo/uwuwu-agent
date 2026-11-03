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
#[cfg(test)]
mod title_tests;

use clap::Parser as _;

pub(crate) use cli::{Cli, Commands, CronSub, TaskSub};

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_gql::AffineUi;
use crate::affine_search_server::ServerSearchConfig;
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
/// (крон-агенты комментят свои тела через тот же task comment). id-матч
/// идёт без сети; для матчинга по подстроке заголовка доска дозаголоввается
/// одним батч-запросом (affine_titles) — тот же паттерн на крон-досках.
pub(crate) async fn resolve_row_any_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: &str,
) -> anyhow::Result<(affine_tasks::Board, affine_tasks::TaskRow)> {
    let board = affine_tasks::find_board(client, cfg, project).await?;
    let mut rows = affine_tasks::read_rows(client, cfg, &board, root).await?;
    let mut decisive: Option<anyhow::Error> = None;
    if let Some(row) = resolve_row_on_board(cfg, &mut rows, id).await? {
        return Ok((board, row));
    }
    // фоллбек: крон-доски (Crons + «Кроны <proj>» всех проектов); решающая
    // диагностика (неоднозначный префикс) запоминается — если нигде не
    // нашлось, юзер получит её, а не дезинформирующее «не найдена»
    for bt in affine_tasks::cron_board_titles(client, cfg).await? {
        if bt == "Crons" {
            continue;
        }
        let Ok(board) = affine_tasks::find_board_by_title(client, cfg, &bt).await else {
            continue;
        };
        let Ok(mut rows) = affine_tasks::read_rows(client, cfg, &board, root).await else {
            continue;
        };
        match resolve_row_on_board(cfg, &mut rows, id).await {
            Ok(Some(row)) => return Ok((board, row)),
            Ok(None) => {}
            Err(e) if decisive.is_none() => decisive = Some(e),
            Err(_) => {}
        }
    }
    let cron_board = affine_tasks::find_board_by_title(client, cfg, "Crons").await?;
    let mut cron_rows = affine_tasks::read_rows(client, cfg, &cron_board, root).await?;
    match resolve_row_on_board(cfg, &mut cron_rows, id).await? {
        Some(row) => Ok((cron_board, row)),
        None => match decisive {
            Some(e) => Err(e.context("искали на доске проекта и всех крон-досках")),
            None => anyhow::bail!(
                "задача '{id}' не найдена (искали на доске проекта и всех крон-досках)"
            ),
        },
    }
}

/// id-матч без сети; промах → один батч-дозаголовок доски и повтор
/// (title-подстрока). Ok(None) — доска не сматчилась; Err — решающая
/// диагностика (неоднозначный префикс), её доносим до юзера, не маскируем.
async fn resolve_row_on_board(
    cfg: &AffineConfig,
    rows: &mut [affine_tasks::TaskRow],
    id: &str,
) -> anyhow::Result<Option<affine_tasks::TaskRow>> {
    match affine_tasks::resolve_row_match(rows, id) {
        affine_tasks::RowMatch::Found(row) => return Ok(Some(row.clone())),
        affine_tasks::RowMatch::Ambiguous(msg) => return Err(anyhow::anyhow!("{msg}")),
        affine_tasks::RowMatch::NotFound => {}
    }
    fill_display_titles(cfg, rows).await;
    match affine_tasks::resolve_row_match(rows, id) {
        affine_tasks::RowMatch::Found(row) => Ok(Some(row.clone())),
        affine_tasks::RowMatch::Ambiguous(msg) => Err(anyhow::anyhow!("{msg}")),
        affine_tasks::RowMatch::NotFound => Ok(None),
    }
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
        let Ok(mut rows) = affine_tasks::read_rows(client, cfg, &board, root).await else {
            continue;
        };
        match resolve_row_on_board(cfg, &mut rows, id).await {
            Ok(Some(row)) => return Ok((board, row)),
            Ok(None) => last_err = format!("задача '{id}' не найдена"),
            Err(e) => last_err = format!("{e}"),
        }
    }
    anyhow::bail!("крон «{id}» не найден на крон-досках ({last_err})")
}

/// Заголовок строки для показа: пустой → плейсхолдер с id тела.
pub(crate) fn display_title(row: &affine_tasks::TaskRow) -> String {
    if !row.title.is_empty() {
        row.title.clone()
    } else if !row.body_doc.is_empty() {
        format!("(тело {})", row.body_doc)
    } else {
        "(без тела)".to_string()
    }
}

/// Дозаголовок одной строки для показа/функций — ровно она, O(1).
/// Лучшее усилие: без UI-кредов или при сбое — плейсхолдер.
pub(crate) async fn row_display_title(
    cfg: &AffineConfig,
    row: &mut affine_tasks::TaskRow,
) -> String {
    if row.title.is_empty() && !row.body_doc.is_empty() {
        if let Ok(ui) = connect_ui().await {
            let _ = affine_tasks::resolve_titles(&ui, cfg, std::slice::from_mut(row)).await;
        }
    }
    display_title(row)
}

/// Строгий дозаголовок (reply docTitle): тайтл обязан быть — иначе Err.
pub(crate) async fn require_row_title(
    ui: &AffineUi,
    cfg: &AffineConfig,
    row: &mut affine_tasks::TaskRow,
) -> anyhow::Result<String> {
    if row.title.is_empty() && !row.body_doc.is_empty() {
        affine_tasks::resolve_titles(ui, cfg, std::slice::from_mut(row)).await?;
    }
    if row.title.is_empty() {
        anyhow::bail!("заголовок задачи недоступен — docTitle ответа не может быть пустым");
    }
    Ok(row.title.clone())
}

/// Дозаголовки набора строк одним батчем — лучшее усилие для показа:
/// нет UI-кредов или сбой запроса → остаются плейсхолдеры.
pub(crate) async fn fill_display_titles(cfg: &AffineConfig, rows: &mut [affine_tasks::TaskRow]) {
    let needs = rows
        .iter()
        .any(|r| r.title.is_empty() && !r.body_doc.is_empty());
    if !needs {
        return;
    }
    let Some(config) = ServerSearchConfig::from_env() else {
        return;
    };
    let Ok(ui) = AffineUi::connect(&config).await else {
        eprintln!("uwuwu-cli: UI GraphQL недоступен — заголовки показываю плейсхолдерами");
        return;
    };
    if let Err(e) = affine_tasks::resolve_titles(&ui, cfg, rows).await {
        eprintln!("uwuwu-cli: батч-тайтлы не удались ({e:#}) — показываю плейсхолдеры");
    }
}

/// UI GraphQL клиент: Err без кредов (честная ошибка для функциональных путей).
pub(crate) async fn connect_ui() -> anyhow::Result<AffineUi> {
    let Some(config) = ServerSearchConfig::from_env() else {
        anyhow::bail!(
            "нужны UI-креды афины (UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD) для заголовков задач"
        );
    };
    AffineUi::connect(&config).await
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
