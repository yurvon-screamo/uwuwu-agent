//! task undo: откат сноса из журнала (тело + клетки).

use crate::cmd::sy_dispatch::cells::set_text_cells;
use crate::markdown;
use crate::siyuan_goal as goal;
use crate::siyuan::SiYuan;
use crate::siyuan_guard as guard;
use crate::siyuan_guard::{Journal, JournalRow};
use crate::siyuan_task as syt;
use anyhow::{bail, Result};
use serde_json::json;

/// Undo: список журналов или идемпотентное восстановление строк.
/// Тайтл уже на доске → no-op; id строк меняются — карта в выводе.
pub(super) async fn task_undo(
    sy: &SiYuan,
    project: &str,
    journal_name: Option<&str>,
) -> Result<()> {
    let Some(name) = journal_name else {
        let journals = Journal::list(sy).await?;
        if journals.is_empty() {
            println!("undo-журналов нет ({})", guard::UNDO_DIR);
            return Ok(());
        }
        println!("undo-журналы ({}):", guard::UNDO_DIR);
        for (name, journal) in &journals {
            println!(
                "  {name}  {} [{}, {} строк(а)]",
                journal.created,
                journal.kind,
                journal.rows.len()
            );
        }
        println!("восстановление: task undo <имя> --project <проект журнала>");
        return Ok(());
    };
    let journal = Journal::load(sy, name).await?;
    if journal.project != project {
        bail!(
            "журнал принадлежит проекту «{}», а --project «{project}»",
            journal.project
        );
    }
    let board = match journal.kind.as_str() {
        // цели: доска «Цели <proj>» (владелец завёл один раз) — не доска задач
        "goal-rm" => goal::find_goal_board(sy, project).await?,
        // легаси "cron-rm" (крон-доски сняты) уводится на задачную доску
        // и честно падает по av-mismatch ниже — восстанавливать вручную
        _ => syt::find_board(sy, project).await?,
    };
    if board.av_id != journal.av_id {
        bail!(
            "доска пересоздана (av {} ≠ {} из журнала) — авто-undo невозможен, восстанавливай вручную",
            board.av_id, journal.av_id
        );
    }
    let chain = syt::board_doc_chain(sy, &board).await?;
    guard::control_probe_board(sy, &board, &chain).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let mut restored = 0usize;
    for row in &journal.rows {
        let present = rows
            .iter()
            .any(|r| syt::normalize_title(&r.title) == syt::normalize_title(&row.title));
        if present {
            println!("  пропущен (тайтл уже на доске): {}", row.title);
            continue;
        }
        let body_doc = restore_journal_body(sy, &board, row).await?;
        let item = if body_doc.is_empty() {
            sy.db_item_add(&board.av_id, &syt::path_safe(&row.title))
                .await?
        } else {
            sy.db_item_add_bound(&board.av_id, &body_doc).await?
        };
        restore_journal_cells(sy, &board, &item, row).await?;
        println!(
            "  восстановлена: «{}» — строка {item} (была {}), тело {}",
            row.title,
            row.item_id,
            if body_doc.is_empty() {
                "—".to_string()
            } else {
                body_doc.clone()
            }
        );
        restored += 1;
    }
    println!(
        "undo: восстановлено {restored}/{}; id строк сменились — старые ссылки обнови (коммент «старый id → новый id»)",
        journal.rows.len()
    );
    Ok(())
}

/// Тело для восстановленной строки: живое переиспользуется, отсутствующее
/// пересоздаётся из журнала (docId меняется); без тела — detached-строка.
async fn restore_journal_body(
    sy: &SiYuan,
    board: &syt::SyBoard,
    row: &JournalRow,
) -> Result<String> {
    let Some(md) = row.body_md.as_deref() else {
        return Ok(String::new());
    };
    if let Some(doc) = row.body_doc.as_deref() {
        if syt::doc_block_exists(sy, doc).await {
            return Ok(doc.to_string());
        }
    }
    let ids = syt::body_doc_by_title(sy, board, &row.title).await?;
    match ids.as_slice() {
        [] => {
            let clean_md = markdown::strip_export_wrapper(md, &row.title);
            sy.create_doc_md(&board.notebook, &row.body_hpath, &clean_md)
                .await
        }
        [only] => Ok(only.clone()),
        many => bail!(
            "несколько доков с заголовком «{}»: {} — вычисти дубли, потом повтори undo",
            row.title,
            many.join(", ")
        ),
    }
}

/// Клетки восстановленной строки: Status (mSelect) + Branch + PR (url).
/// Легаси "cron-rm"-журналы (крон-доски сняты) не восстанавливаются:
/// undo уводится на задачную доску и честно падает по av-mismatch.
async fn restore_journal_cells(
    sy: &SiYuan,
    board: &syt::SyBoard,
    item: &str,
    row: &JournalRow,
) -> Result<()> {
    if !row.status.is_empty() {
        let sk = syt::status_key(sy, &board.av_id).await?;
        sy.db_item_set_status(&board.av_id, &sk, item, &row.status)
            .await?;
    }
    if !row.branch.is_empty() {
        set_text_cells(sy, &board.av_id, item, &[("Branch", row.branch.as_str())]).await?;
    }
    if row.pr.is_empty() {
        return Ok(());
    }
    let keys = sy
        .mcp_tool("database", json!({ "action": "keys", "id": board.av_id }))
        .await?;
    let pr_key = keys
        .pointer("/data/keys")
        .or_else(|| keys.get("keys"))
        .and_then(|k| k.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|k| k.get("name").and_then(|n| n.as_str()) == Some("PR"))
                .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
        });
    match pr_key {
        Some(key) => {
            let value = syt::pr_cell_value(&row.pr)?;
            sy.mcp_tool(
                "database",
                json!({ "action": "item_update", "id": board.av_id, "keyID": key,
                        "itemID": item, "value": value }),
            )
            .await?;
            Ok(())
        }
        None => bail!(
            "ключ PR не найден на доске — PR «{}» не восстановлен",
            row.pr
        ),
    }
}
