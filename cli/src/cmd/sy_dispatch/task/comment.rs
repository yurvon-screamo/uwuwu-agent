//! task comment/thread: вставка коммента (дедуп-хит — отдельная честная
//! строка) и печать тредов тела.

use crate::cmd::cli::TaskSub;
use crate::md_artifacts;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::Result;

use super::helpers::resolve_task;
use crate::cmd::sy_dispatch::guards::guarded_comment_text;
use crate::cmd::sy_dispatch::body_comments::print_threads;

pub(super) async fn run_comment(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Comment {
        id,
        reply,
        stdin,
        md,
        text,
    } = command
    else {
        unreachable!("task_run роутит Comment в comment::run_comment");
    };
    let msg = guarded_comment_text(stdin, md.as_deref(), &text)?;
    // write-side превенция тег-матча ядра (см. body): предупреждение
    // в stderr, запись не блокируется
    if let Some(warning) = md_artifacts::tag_trap_warning(&msg) {
        eprintln!("{warning}");
    }
    let (board, task) = resolve_task(sy, project, &id).await?;
    let inserted = syt::comment(sy, &board, &task, "AGENTS", &msg, reply.as_deref()).await?;
    // контракт ответов: вставленный блок как записался; дедуп-хит
    // (бит-в-бит дубль уже был) — отдельная честная строка
    if inserted.fresh {
        println!("коммент записан на {}:\n{}", board.hpath, inserted.marker);
    } else {
        println!(
            "коммент уже был (бит-в-бит дубль, вставки не было) на {}:\n{}",
            board.hpath, inserted.marker
        );
    }
    Ok(())
}

pub(super) async fn run_thread(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Thread { id } = command else {
        unreachable!("task_run роутит Thread в comment::run_thread");
    };
    let (board, task) = resolve_task(sy, project, &id).await?;
    let md = match syt::task_body_doc(sy, &board, &task).await? {
        Some(doc) => sy.export_md_by_id(&doc).await.unwrap_or_default(),
        None => String::new(),
    };
    print_threads(&md);
    Ok(())
}
