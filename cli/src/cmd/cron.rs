//! `uwuwu-cli cron`: durable agent schedules on AFFiNE cron boards.

use anyhow::Context as _;
use serde_json::json;

use super::{resolve_cron_any_board, truncate, CronResolveFail, CronSub};
use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_tasks;

pub(crate) async fn run(
    cfg: &AffineConfig,
    root: &std::path::Path,
    command: CronSub,
) -> anyhow::Result<()> {
    let mut client = AffineClient::connect(cfg).await?;
    // крон-доски: глобальная «Crons» + «Кроны <proj>» по папкам Projects
    let cron_boards = affine_tasks::cron_board_titles(&mut client, cfg).await?;
    let is_pause = matches!(command, CronSub::Pause { .. });
    match command {
        CronSub::Set {
            title,
            r#in,
            every,
            at,
            cron_expr,
            project,
            stdin,
        } => {
            set(
                &mut client,
                cfg,
                root,
                SetArgs {
                    title,
                    r#in,
                    every,
                    at,
                    cron_expr,
                    project,
                    stdin,
                },
            )
            .await
        }
        CronSub::List => list(&mut client, cfg, root, &cron_boards).await,
        CronSub::Pause { id } | CronSub::Resume { id } => {
            let status = if is_pause { "paused" } else { "active" };
            let (board, row) =
                match resolve_cron_any_board(&mut client, cfg, &cron_boards, root, &id).await {
                    Ok(pair) => pair,
                    Err(super::CronResolveFail::Other(e)) => return Err(e),
                    Err(fail) => anyhow::bail!("крон «{id}» {fail}"),
                };
            let patch = affine_tasks::RowPatch::cron_status(status);
            affine_tasks::edit_row(&mut client, cfg, &board, &row, &patch).await?;
            println!("статус: {status}");
            Ok(())
        }
        CronSub::Rm { id } => {
            match resolve_cron_any_board(&mut client, cfg, &cron_boards, root, &id).await {
                Ok((board, mut row)) => {
                    // тайтл эха — до удаления строки/тела (O(1)-дозаголовок)
                    let title = super::row_display_title(cfg, &mut row).await;
                    client
                        .call_tool("delete_database_row", json!({"workspaceId": cfg.workspace, "docId": board.doc_id, "databaseBlockId": board.db_id, "rowBlockId": row.row_id}))
                        .await
                        .context("delete_database_row")?;
                    if !row.body_doc.is_empty() {
                        // честный трэш: строка уже удалена, но ложное «удалён»
                        // при живом теле-призраке недопустимо — предупреждение
                        let mut cx = affine_tasks::TaskCx {
                            client: &mut client,
                            cfg,
                            board: &board,
                            root,
                        };
                        if let Err(e) = affine_tasks::trash_body(&mut cx, &row.body_doc).await {
                            eprintln!(
                                "внимание: строка удалена, но тело {} НЕ утрэшено: {e}",
                                row.body_doc
                            );
                        }
                    }
                    println!("крон удалён: {title}");
                    Ok(())
                }
                // однозначное «не найдено» на всех читаемых досках —
                // безопасно идти в сирота-путь (тело крона без строки)
                Err(CronResolveFail::NotFound {
                    details,
                    all_readable: true,
                }) => match affine_tasks::rm_cron_orphan(&mut client, cfg, &cron_boards, &id).await
                {
                    Ok(title) => {
                        println!("крон-сирота в трэше: {title}");
                        Ok(())
                    }
                    Err(orphan_err) => {
                        anyhow::bail!("крон «{id}» не найден ({details}); {orphan_err}")
                    }
                },
                Err(fail) => Err(anyhow::anyhow!("{fail}")),
            }
        }
    }
}

struct SetArgs {
    title: String,
    r#in: Option<String>,
    every: Option<String>,
    at: Option<String>,
    cron_expr: Option<String>,
    project: Option<String>,
    stdin: bool,
}

async fn set(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    args: SetArgs,
) -> anyhow::Result<()> {
    let board = match args.project.as_deref() {
        Some(proj) => {
            let board_title = format!("Кроны {proj}");
            affine_tasks::find_board_by_title(client, cfg, &board_title)
                .await
                .with_context(|| {
                    format!("доска «{board_title}» не найдена — создай док-таблицу в папке проекта «{proj}»")
                })?
        }
        None => affine_tasks::find_board_by_title(client, cfg, "Crons")
            .await
            .context("доска «Crons» не найдена")?,
    };
    let schedule = pick_schedule(&args)?;
    affine_tasks::validate_schedule(&schedule)?;
    let prompt = if args.stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        String::new()
    };
    let body_md = affine_tasks::cron_body_stub(&args.title, &schedule, &prompt);
    let mut cx = affine_tasks::TaskCx {
        client,
        cfg,
        board: &board,
        root,
    };
    let (row, outcome) =
        affine_tasks::create_cron(&mut cx, &args.title, &schedule, &body_md).await?;
    match outcome {
        affine_tasks::CreateOutcome::Created => println!("крон поставлен: {}", row.title),
        affine_tasks::CreateOutcome::Recovered => {
            println!("крон достроен после сбоя (сирота доски): {}", row.title)
        }
    }
    println!("  расписание: {schedule}");
    println!("  строка: {}", row.row_id);
    println!("  тело:   {}", row.body_doc);
    Ok(())
}

/// Ровно один из --in/--every/--at/--cron; вернуть его значение.
fn pick_schedule(args: &SetArgs) -> anyhow::Result<String> {
    let specs = [
        args.r#in.as_ref(),
        args.every.as_ref(),
        args.at.as_ref(),
        args.cron_expr.as_ref(),
    ];
    let n = specs.iter().flatten().count();
    if n != 1 {
        anyhow::bail!("ровно один из --in/--every/--at/--cron (передано {n})");
    }
    if let Some(off) = &args.r#in {
        Ok(affine_tasks::one_shot_schedule(
            affine_tasks::parse_offset_minutes(off)?,
        ))
    } else if let Some(e) = &args.every {
        Ok(e.clone())
    } else if let Some(a) = &args.at {
        Ok(a.clone())
    } else {
        Ok(args.cron_expr.clone().unwrap_or_default())
    }
}

async fn list(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &std::path::Path,
    cron_boards: &[String],
) -> anyhow::Result<()> {
    println!(
        "{:<14} {:<8} {:<18} {:<17} {:<13} ЗАГОЛОВОК",
        "ДОСКА", "СТАТУС", "РАСПИСАНИЕ", "LAST RUN", "СТРОКА"
    );
    let mut summary: Vec<String> = Vec::new();
    for board_title in cron_boards {
        // полный статус каждой доски: пустые/трэш/отсутствующие
        // тоже видны — молчаливые пропуски читаются как баг
        match affine_tasks::board_lookup(client, cfg, board_title).await {
            Ok(affine_tasks::BoardFind::Found(board)) => {
                let mut rows = affine_tasks::read_rows(client, cfg, &board, root).await?;
                super::fill_display_titles(cfg, &mut rows).await;
                summary.push(format!("{board_title}: {}", rows.len()));
                for r in &rows {
                    println!(
                        "{:<14} {:<8} {:<18} {:<17} {:<13} {}",
                        truncate(board_title, 13),
                        r.status,
                        r.cron_schedule_of(),
                        r.last_run_display(),
                        &r.row_id[..r.row_id.len().min(12)],
                        truncate(&super::display_title(r), 36)
                    );
                }
            }
            Ok(affine_tasks::BoardFind::Trashed) => {
                if board_title == "Crons" {
                    summary.push("Crons: в трэше (глобальная не используется)".into());
                } else {
                    summary.push(format!(
                        "{board_title}: доска в трэше — восстанови или пересоздай"
                    ));
                }
            }
            Ok(affine_tasks::BoardFind::Missing) => {
                if board_title == "Crons" {
                    summary.push("Crons: глобальной доски нет (не используется)".into());
                } else {
                    summary.push(format!("{board_title}: доски нет — создай в папке проекта"));
                }
            }
            Err(e) => summary.push(format!("{board_title}: ошибка поиска ({e:#})")),
        }
    }
    if !summary.is_empty() {
        println!();
        println!("доски: {}", summary.join(" · "));
    }
    Ok(())
}
