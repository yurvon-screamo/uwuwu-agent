//! task list/get: перечисление доски и шапка+тело задачи (контракт get —
//! громкий резолв тела, аттачи, round-trip-артефакты).

use crate::cmd::sy_dispatch::truncate;
use crate::cmd::cli::TaskSub;
use crate::md_artifacts;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::Result;

use super::helpers::{resolve_task, task_header_md_with_doc};
use crate::cmd::sy_dispatch::attach::save_body_assets;

pub(super) async fn run_list(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::List {
        status, r#type: ty, ..
    } = command
    else {
        unreachable!("task_run роутит List в list_get::run_list");
    };
    // мусорный тип фильтра — громкий отказ через тот же канон,
    // а не тихая пустота «Задач нет.» (паритет create/edit)
    let ty = match ty.as_deref() {
        Some(raw) => Some(syt::validate_task_type(raw)?),
        None => None,
    };
    let board = syt::find_board(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let dead_bound = syt::dead_bound_doc_ids(sy, &rows).await?;
    let mut filtered: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            status
                .as_deref()
                .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                .unwrap_or(true)
        })
        .filter(|r| {
            ty.as_deref()
                .map(|t| r.task_type.eq_ignore_ascii_case(t))
                .unwrap_or(true)
        })
        .collect();
    if filtered.is_empty() {
        println!("Задач нет.");
    }
    println!(
        "{:<13} {:<6} {:<7} {:<3} {:<12} {:<34} PR",
        "СТАТУС", "ТИП", "ПРИОР", "AD", "СТРОКА", "ЗАГОЛОВОК"
    );
    filtered.sort_by(|a, b| a.status.cmp(&b.status));
    for r in &filtered {
        let marker = match r.body_doc.as_deref() {
            Some(b) if dead_bound.contains(b) => " (!)",
            _ => "",
        };
        println!(
            "{:<13} {:<6} {:<7} {:<3} {:<12} {:<34} {}{}",
            r.status,
            r.task_type,
            r.priority,
            if r.autodone { "on" } else { "" },
            &r.item_id[..r.item_id.len().min(12)],
            truncate(&r.title, 32),
            r.pr,
            marker
        );
    }
    Ok(())
}

pub(super) async fn run_get(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Get { id, no_body } = command else {
        unreachable!("task_run роутит Get в list_get::run_get");
    };
    let (board, task) = resolve_task(sy, project, &id).await?;
    // один резолв тела (task_body_doc громкий: fallback по заголовку,
    // отказ при телах-дублях) — шапка и экспорт берут один и тот же док,
    // warning мёртвой привязки печатается ровно один раз
    let body_doc = syt::task_body_doc(sy, &board, &task).await?;
    let header = task_header_md_with_doc(sy, &board, &task, body_doc.as_deref()).await?;
    print!("{header}");
    if !no_body {
        let md = match body_doc {
            Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
            None => String::new(),
        };
        for warning in md_artifacts::artifact_report(&md) {
            eprintln!("⚠ round-trip-артефакт: {warning}");
        }
        println!("\n----- тело -----\n{md}");
        let assets = save_body_assets(sy, &md, &task.item_id).await;
        if !assets.is_empty() {
            println!("Аттачи:");
            for path in assets {
                println!("  {path}");
            }
        }
    }
    Ok(())
}
