//! task rm + журнал-вердикт: row_verdict, journal_row, task_rm.

use super::helpers::resolve_task;
use crate::siyuan::SiYuan;
use crate::siyuan_guard as guard;
use crate::siyuan_guard::{Journal, JournalRow, Probe, Verdict};
use crate::siyuan_task as syt;
use anyhow::{bail, Result};
use serde_json::json;

/// Вердикт строки по независимым источникам: FS .sy тела, SQL по id,
/// док по тайтлу; для Draft с живым телом — экспорт против скелетов.
/// `chain` — цепочка doc-id доски (пути FS-проб).
pub(super) async fn row_verdict(
    sy: &SiYuan,
    board: &syt::SyBoard,
    chain: &[String],
    task: &syt::SyTask,
) -> Result<Verdict> {
    let title_ids = syt::body_doc_by_title(sy, board, &task.title).await?;
    let fs = guard::probe_body_fs(sy, board, chain, task).await;
    let sql = guard::probe_body_sql(sy, task).await;
    let draft_body_export = match (fs, task.body_doc.as_deref()) {
        (Probe::Found, Some(doc)) if task.status.eq_ignore_ascii_case("draft") => {
            sy.export_md_by_id(doc).await.ok()
        }
        _ => None,
    };
    let evidence =
        guard::evidence_from_sources(task, title_ids.len(), fs, sql, draft_body_export.as_deref());
    Ok(guard::verdict(&evidence))
}

/// Строка undo-журнала: клетки + экспорт тела ДО сноса (None — тела нет).
pub(crate) async fn journal_row(
    sy: &SiYuan,
    board: &syt::SyBoard,
    task: &syt::SyTask,
) -> Result<JournalRow> {
    let body_md = match task.body_doc.as_deref() {
        Some(doc) => sy.export_md_by_id(doc).await.ok(),
        None => None,
    };
    Ok(JournalRow {
        item_id: task.item_id.clone(),
        title: task.title.clone(),
        status: task.status.clone(),
        branch: task.branch.clone(),
        pr: task.pr.clone(),
        extra_text_cells: Vec::new(),
        body_doc: task.body_doc.clone(),
        body_hpath: board.body_hpath(&task.title),
        body_md,
    })
}

/// rm одной задачи под guard'ом: вердикт по источникам, отказ для живых
/// не-Draft (задекларированный контракт команды), write-ahead журнал.
/// Id-команда: без --project резолвит глобально; имя проекта для журнала
/// при этом выводится из доски (undo-валидация остаётся проектной).
pub(super) async fn task_rm(sy: &SiYuan, project: &str, id: &str) -> Result<()> {
    let (board, task) = resolve_task(sy, project, id).await?;
    let project = if project.is_empty() {
        syt::board_project(&board)
    } else {
        project.to_string()
    };
    let chain = syt::board_doc_chain(sy, &board).await?;
    // Строка резолвилась на доске — повторный resolve не нужен: вердикт
    // строится по найденной задаче (resolve внутри task_rm дублировал бы
    // render; контракт guard'а — вердикт по независимым источникам, не
    // по повторному матчингу).
    match row_verdict(sy, &board, &chain, &task).await? {
        Verdict::Divergent => bail!(
            "состояние строки «{}» неясно (FS/SQL-проба не состоялась) — rm отклонён, повтори позже",
            task.title
        ),
        Verdict::Live if !task.status.eq_ignore_ascii_case("draft") => bail!(
            "задача «{}» жива (статус «{}», тело существует): rm только для Draft-черновиков; \
             живую закрывай task close, owner-approved снос — task clean --yes --live",
            task.title, task.status
        ),
        _ => {}
    }
    let mut journal = Journal::new(&project, "rm", &board);
    journal.rows.push(journal_row(sy, &board, &task).await?);
    let journal_path = journal.save(sy).await?;
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [task.item_id] }),
    )
    .await?;
    if let Some(doc) = syt::task_body_doc(sy, &board, &task).await? {
        let _ = sy.remove_doc(&doc).await;
    }
    println!("удалена: {} (undo-журнал: {journal_path})", task.title);
    Ok(())
}

/// Исполнение сноса: write-ahead журнал, один item_remove массивом,
/// трэш скелетных тел, пост-чек состава доски.
pub(super) async fn execute_removal(
    sy: &SiYuan,
    board: &syt::SyBoard,
    project: &str,
    removable: &[(&syt::SyTask, Verdict)],
    rows_before: &[syt::SyTask],
) -> Result<()> {
    // write-ahead: журнал обязан лежать в воркспейсе до первой мутации
    let mut journal = Journal::new(project, "clean", board);
    for (task, _) in removable {
        journal.rows.push(journal_row(sy, board, task).await?);
    }
    let journal_path = journal.save(sy).await?;

    let item_ids: Vec<&str> = removable.iter().map(|(t, _)| t.item_id.as_str()).collect();
    sy.mcp_tool(
        "database",
        json!({ "action": "item_remove", "id": board.av_id, "itemIDs": item_ids }),
    )
    .await?;
    // тела: DraftSkeleton — пустой скелет в трэш; Live — тело НЕ трогается
    // (восстановимый сирота, практика спасения строк 04.11); Dead — тела нет
    for (task, verdict) in removable {
        if *verdict == Verdict::DraftSkeleton {
            if let Some(doc) = task.body_doc.as_deref() {
                let _ = sy.remove_doc(doc).await;
            }
        }
    }

    let rows_after = syt::read_rows(sy, board).await?;
    let before_titles: Vec<String> = rows_before.iter().map(|r| r.title.clone()).collect();
    let after_titles: Vec<String> = rows_after.iter().map(|r| r.title.clone()).collect();
    let removed_titles: Vec<String> = removable.iter().map(|(t, _)| t.title.clone()).collect();
    let extra = guard::extra_missing_titles(&before_titles, &after_titles, &removed_titles);
    if !extra.is_empty() {
        bail!(
            "ПОСТ-ЧЕК: с доски пропали строки ВНЕ плана ({:?}) — возможна потеря живых задач; \
             восстановление — task undo {} --project {project}",
            extra,
            journal_path
        );
    }
    println!(
        "снесено строк: {}; undo-журнал: {journal_path}; пост-чек: состав доски ок",
        removable.len()
    );
    Ok(())
}
