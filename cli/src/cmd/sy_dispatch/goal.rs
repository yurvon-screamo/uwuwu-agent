//! goal: реестр долгосрочных целей («Цели <proj>»).

use crate::cmd::sy_dispatch::guards::read_stdin;
use crate::cmd::sy_dispatch::task::helpers::{print_final_body, reload_task, warn_unreadable_state};
use crate::cmd::sy_dispatch::task::journal_row;
use crate::cmd::sy_dispatch::truncate;
use crate::cmd::cli::GoalSub;
use crate::siyuan_goal as goal;
use crate::siyuan::SiYuan;
use crate::siyuan_guard::Journal;
use crate::siyuan_task as syt;
use crate::siyuan_task::SyTask;
use serde_json::json;

/// Диспетчер цели-семьи: доску «Цели <proj>» заводит владелец руками в UI —
/// CLI без неё громко отказывается с инструкцией (find_goal_board).
pub(crate) async fn goal_run(project: &str, command: GoalSub) -> anyhow::Result<()> {
    if project.is_empty() {
        anyhow::bail!("--project <имя> обязателен (uwuwu | origa | …)");
    }
    let sy = SiYuan::from_env()?;
    match command {
        GoalSub::List { status } => goal_list_run(&sy, project, status).await,
        GoalSub::Add {
            title,
            status,
            stdin,
        } => goal_add_run(&sy, project, &title, status, stdin).await,
        GoalSub::Edit { id, status, title } => {
            goal_edit_run(&sy, project, &id, status, title).await
        }
        GoalSub::Rm { id } => goal_rm(&sy, project, &id).await,
    }
}

async fn goal_list_run(sy: &SiYuan, project: &str, status: Option<String>) -> anyhow::Result<()> {
    let board = goal::find_goal_board(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let dead_bound = syt::dead_bound_doc_ids(sy, &rows).await?;
    let filtered: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            status
                .as_deref()
                .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                .unwrap_or(true)
        })
        .collect();
    let table = format_goal_table(&filtered, &dead_bound);
    println!("{table}");
    Ok(())
}

async fn goal_add_run(
    sy: &SiYuan,
    project: &str,
    title: &str,
    status: Option<String>,
    stdin: bool,
) -> anyhow::Result<()> {
    if title.trim().is_empty() {
        anyhow::bail!("пустой тайтл — передай непустую строку в --title");
    }
    let ctx = if stdin { read_stdin()? } else { String::new() };
    let canonical = status.as_deref().map(goal::canonical_status).transpose()?;
    let (item, body) = goal::create(
        sy,
        project,
        title,
        canonical.as_deref().unwrap_or("todo"),
        &ctx,
    )
    .await?;
    // контракт ответов: созданная цель целиком (карточка перечитанной
    // строки + финальный экспорт тела); сбой перечитывания → ADR-067
    match goal::find_goal_board(sy, project).await {
        Ok(board) => match reload_task(sy, &board, &item).await {
            Ok(fresh) => {
                println!("{}", format_goal_card(&board, &fresh));
                print_final_body(sy, &body, "цель (итог)").await;
            }
            Err(err) => warn_unreadable_state(&item, &err),
        },
        Err(err) => warn_unreadable_state(&item, &err),
    }
    Ok(())
}

async fn goal_edit_run(
    sy: &SiYuan,
    project: &str,
    id: &str,
    status: Option<String>,
    title: Option<String>,
) -> anyhow::Result<()> {
    if status.is_none() && title.is_none() {
        anyhow::bail!("нечего менять: передай --status и/или --title");
    }
    let board = goal::find_goal_board(sy, project).await?;
    let goal = syt::resolve_entity(sy, &board, id, "цель").await?;
    // канонизация статуса до любых записей: без частичных обновлений
    let canonical = status.as_deref().map(goal::canonical_status).transpose()?;
    if let Some(st) = canonical.as_deref() {
        syt::edit(sy, &board, &goal, &syt::CellUpdates::status_only(st)).await?;
    }
    if let Some(raw) = title.as_deref() {
        syt::edit_title(sy, &board, &goal, raw).await?;
    }
    // контракт ответов: финальное состояние строки цели (шапка цели)
    match reload_task(sy, &board, &goal.item_id).await {
        Ok(fresh) => println!("{}", format_goal_card(&board, &fresh)),
        Err(err) => warn_unreadable_state(&goal.item_id, &err),
    }
    Ok(())
}

/// Карточка цели (контракт ответов goal add/edit): статус → строка →
/// заголовок → доска. Формат парный шапке task-карточки.
pub(crate) fn format_goal_card(board: &syt::SyBoard, goal: &SyTask) -> String {
    format!(
        "Цель:     {}\nСтатус:   {}\nСтрока:   {}\nДоска:    {}",
        goal.title, goal.status, goal.item_id, board.hpath
    )
}

/// Таблица реестра целей: статус → строка → заголовок; `(!)` — мёртвая
/// привязка тела (канон task list). Строки сортируются по статусу
/// (группировка канбана: done-цели не теряются среди активных).
pub(crate) fn format_goal_table(
    rows: &[syt::SyTask],
    dead_bound: &std::collections::HashSet<String>,
) -> String {
    if rows.is_empty() {
        return "Целей нет.".to_string();
    }
    let mut sorted: Vec<&syt::SyTask> = rows.iter().collect();
    // канонический порядок статусов (лестница прогресса todo → in progress →
    // done), не алфавит: группы канбана читаются сверху вниз по фазе цели;
    // вне-канонные статусы — в конец
    sorted.sort_by_key(|r| {
        goal::GOAL_STATUSES
            .iter()
            .position(|canon| canon.eq_ignore_ascii_case(&r.status))
            .unwrap_or(goal::GOAL_STATUSES.len())
    });
    let mut out = String::new();
    out.push_str(&format!(
        "{:<13} {:<12} {:<34}\n",
        "СТАТУС", "СТРОКА", "ЦЕЛЬ"
    ));
    for r in sorted {
        let marker = match r.body_doc.as_deref() {
            Some(b) if dead_bound.contains(b) => " (!)",
            _ => "",
        };
        out.push_str(&format!(
            "{:<13} {:<12} {:<34}{}\n",
            r.status,
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 32),
            marker
        ));
    }
    out
}

/// Снос цели: write-ahead undo-журнал → item_remove строки → тело в трэш
/// (паттерн cron rm; Draft-вердикт task rm тут не нужен — цели всегда Live).
async fn goal_rm(sy: &SiYuan, project: &str, id: &str) -> anyhow::Result<()> {
    let board = goal::find_goal_board(sy, project).await?;
    let goal = syt::resolve_entity(sy, &board, id, "цель").await?;
    let mut journal = Journal::new(project, "goal-rm", &board);
    journal.rows.push(journal_row(sy, &board, &goal).await?);
    let journal_path = journal.save(sy).await?;
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [goal.item_id] }),
    )
    .await?;
    if let Some(doc) = syt::task_body_doc(sy, &board, &goal).await? {
        let _ = sy.remove_doc(&doc).await;
    }
    println!("удалена цель: {} (undo-журнал: {journal_path})", goal.title);
    Ok(())
}
