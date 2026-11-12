//! task-семья: диспетчер TaskSub по веткам (create/list_get/edit/comment/
//! body_cmd + журнал journal/clean/undo); общие хелперы — helpers.rs.

mod body_cmd;
mod create_sync;
mod clean;
mod comment;
mod create;
mod edit;
pub(super) mod helpers;
mod journal;
mod list_get;
mod undo;

use crate::cmd::cli::TaskSub;
use anyhow::Result;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use super::guards::{pre_guard_positional_comment, read_stdin};

pub(crate) use journal::journal_row;

/// Dry-run перехват: скелет тела печатается без записей в воркспейс и без
/// доступа к ядру (ноль запросов). Вызывается task_run на Create+dry_run.
pub(super) async fn dry_run(command: &TaskSub) -> Result<bool> {
    let TaskSub::Create {
        title,
        r#type: ty,
        priority,
        autodone,
        stdin,
        attach,
        schedule,
        agent,
        status,
        dry_run: true,
    } = command
    else {
        return Ok(false);
    };
    if !attach.is_empty() {
        anyhow::bail!("create --dry-run не аттачит: скелет печатается без записей в воркспейс — убери --attach");
    }
    // --type валидируется и в осмотре: флаг не молчит (ревью Low);
    // дефолт — DEFAULT_TASK_TYPE (feature), не выпиленный dev
    let ty = match ty.as_deref() {
        Some(raw) => syt::validate_task_type(raw)?,
        None => syt::DEFAULT_TASK_TYPE.to_string(),
    };
    // --priority валидируется и в осмотре: флаг не молчит (паритет --type)
    if let Some(raw) = priority.as_deref() {
        syt::validate_priority(raw)?;
        println!(
            "приоритет: {raw} (будет записан в колонку «{}» доски)",
            syt::PRIORITY_CELL
        );
    }
    // --autodone не молчит (паритет --priority, ревью Low)
    if *autodone {
        println!(
            "autodone: on (будет записан в колонку «{}» доски)",
            syt::AUTODONE_CELL
        );
    }
    // --schedule валидируется и в осмотре: флаг не молчит (ревью Low)
    if let Some(expr) = schedule.as_deref() {
        crate::schedule::validate_schedule(expr)?;
        println!("расписание: {expr} (будет записано атрибутом custom-schedule дока-тела)");
    }
    // --agent валидируется и в осмотре: флаг не молчит (паритет schedule)
    if let Some(name) = agent.as_deref() {
        if name.is_empty() {
            anyhow::bail!("на create пустой агент бессмысленен — убери --agent; снятие: task edit <id> --agent \"\"");
        }
        syt::validate_agent_name(name)?;
        println!("агент: {name} (будет записан атрибутом custom-agent дока-тела)");
    }
    // --status валидируется и в осмотре: флаг не молчит (паритет --type)
    let status_canonical = syt::validate_create_status(status.as_deref())?;
    if status_canonical != "Draft" {
        println!("статус: {status_canonical} (начальный статус строки)");
    }
    println!("тип: {ty} (колонка «{}» доски)", syt::TYPE_CELL);
    let ctx = if *stdin { read_stdin()? } else { String::new() };
    println!("{}", syt::create_body_md(title, &ty, &ctx));
    Ok(true)
}

pub(crate) async fn task_run(project: &str, command: TaskSub) -> Result<()> {
    // dry-run ветка раньше project-проверки и from_env: скелет тела от проекта
    // не зависит, осмотр — read-only и работает без доступа к ядру (ноль запросов)
    if dry_run(&command).await? {
        return Ok(());
    }
    // Гвард позиционного текста коммента ДО bail по --project: иначе флаг,
    // проглоченный trailing_var_arg'ом (`task comment <id> "текст" --project X`
    // → project=None), маскируется под «флаг не передан» (крон-сессия 04.11).
    // stdin здесь НЕ читается — его читает ровно одна ветка ниже; --md тоже
    // не читается — явный канал, гвардится в Comment-ветке.
    if let TaskSub::Comment {
        stdin, md, text, ..
    } = &command
    {
        pre_guard_positional_comment(*stdin, md.as_deref(), text)?;
    }
    // Board-bound подкоманды привязаны к доске/журналу — глобальной семантики
    // нет. Id-семейство (get/edit/claim/comment/thread/close/body/rm) без
    // --project резолвит глобально: resolve_task → resolve_global (доски всех
    // проектов, уникальность обязательна).
    if project.is_empty()
        && matches!(
            command,
            TaskSub::List { .. }
                | TaskSub::Create { .. }
                | TaskSub::Clean { .. }
                | TaskSub::Undo { .. }
        )
    {
        anyhow::bail!("--project <имя> обязателен для list/create/clean/undo (uwuwu | origa | …)");
    }
    let sy = SiYuan::from_env()?;
    match command {
        TaskSub::List { .. } => list_get::run_list(&sy, project, command).await,
        TaskSub::Get { .. } => list_get::run_get(&sy, project, command).await,
        TaskSub::Create { .. } => create::run(&sy, project, command).await,
        TaskSub::Edit { .. } => edit::run_edit(&sy, project, command).await,
        TaskSub::Claim { .. } => edit::run_claim(&sy, project, command).await,
        TaskSub::Comment { .. } => comment::run_comment(&sy, project, command).await,
        TaskSub::Thread { .. } => comment::run_thread(&sy, project, command).await,
        TaskSub::Close { .. } => edit::run_close(&sy, project, command).await,
        TaskSub::Rm { id } => journal::task_rm(&sy, project, &id).await,
        TaskSub::Clean { ids, yes, live } => {
            clean::task_clean(&sy, project, &ids, yes, live.as_deref()).await
        }
        TaskSub::Undo { journal, list } => {
            // --list сильнее позиционного (задаём список, имя не теряем — тест фиксирует)
            let name = if list { None } else { journal.as_deref() };
            undo::task_undo(&sy, project, name).await
        }
        TaskSub::Body { .. } => body_cmd::run_body(&sy, project, command).await,
    }
}
