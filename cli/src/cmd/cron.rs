//! `uwuwu-cli cron`: durable agent schedules on AFFiNE cron boards.

use anyhow::Context as _;
use serde_json::json;

use super::{resolve_cron_any_board, truncate, CronSub};
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
                resolve_cron_any_board(&mut client, cfg, &cron_boards, root, &id).await?;
            let patch = affine_tasks::RowPatch::cron_status(status);
            affine_tasks::edit_row(&mut client, cfg, &board, &row, &patch).await?;
            println!("статус: {status}");
            Ok(())
        }
        CronSub::Rm { id } => {
            let (board, row) =
                resolve_cron_any_board(&mut client, cfg, &cron_boards, root, &id).await?;
            client
                .call_tool("delete_database_row", json!({"workspaceId": cfg.workspace, "docId": board.doc_id, "databaseBlockId": board.db_id, "rowBlockId": row.row_id}))
                .await
                .context("delete_database_row")?;
            if !row.body_doc.is_empty() {
                let _ = client
                    .call_tool(
                        "trash_doc",
                        json!({"workspaceId": cfg.workspace, "docId": row.body_doc}),
                    )
                    .await;
            }
            println!("крон удалён: {}", row.title);
            Ok(())
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

async fn set(client: &mut AffineClient, cfg: &AffineConfig, args: SetArgs) -> anyhow::Result<()> {
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
    let resp = client
        .call_tool(
            "create_doc_from_markdown",
            json!({"workspaceId": cfg.workspace, "title": args.title, "markdown": body_md, "parentDocId": board.doc_id}),
        )
        .await
        .context("create_doc_from_markdown")?;
    let body = resp
        .get("docId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let _ = client
        .call_tool(
            "add_tag_to_doc",
            json!({"workspaceId": cfg.workspace, "docId": body, "tag": affine_tasks::CRON_TAG}),
        )
        .await;
    let row = client
        .call_tool(
            "add_database_row",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                   "databaseBlockId": board.db_id,
                   "cells": {"Title": args.title, "Schedule": schedule, "Status": "active"},
                   "linkedDocId": body}),
        )
        .await
        .context("add_database_row")?;
    println!("крон поставлен: {}", args.title);
    println!("  расписание: {schedule}");
    println!(
        "  строка: {}",
        row.get("rowBlockId")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    );
    println!("  тело:   {body}");
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
                let rows = affine_tasks::read_rows(client, cfg, &board, root).await?;
                summary.push(format!("{board_title}: {}", rows.len()));
                for r in &rows {
                    println!(
                        "{:<14} {:<8} {:<18} {:<17} {:<13} {}",
                        truncate(board_title, 13),
                        r.status,
                        r.cron_schedule_of(),
                        r.last_run_display(),
                        &r.row_id[..r.row_id.len().min(12)],
                        truncate(&r.title, 36)
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
