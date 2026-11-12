//! task edit/claim/close: колонки строки, атрибуты (schedule/agent/planned),
//! заявка сессии, закрытие. Контракт ответов: шапка перечитанной строки.

use crate::cmd::cli::TaskSub;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{anyhow, Context, Result};

use super::helpers::{print_final_state, resolve_task};

pub(super) async fn run_edit(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Edit {
        id,
        status,
        title,
        pr,
        branch,
        r#type: ty,
        priority,
        autodone,
        schedule,
        agent,
        planned,
    } = command
    else {
        unreachable!("task_run роутит Edit в edit::run_edit");
    };
    let (board, task) = resolve_task(sy, project, &id).await?;
    // валидация PR/типа/приоритета/статуса/planned до rename: без
    // частичных обновлений (тайтл сменился, а колонки нет —
    // недопустимый разрыв); «» — снятие типа/приоритета/даты
    pr.as_deref().map(syt::pr_cell_value).transpose()?;
    let ty_canonical = match ty.as_deref() {
        Some("") => Some(String::new()),
        Some(raw) => Some(syt::validate_task_type(raw)?),
        None => None,
    };
    let priority_canonical = match priority.as_deref() {
        Some("") => Some(String::new()),
        Some(raw) => Some(syt::validate_priority(raw)?),
        None => None,
    };
    // on/off: без значения — on (default_missing_value), мусор — отказ
    let autodone_value = match autodone.as_deref() {
        None => None,
        Some("on") | Some("true") | Some("1") => Some(true),
        Some("off") | Some("false") | Some("0") => Some(false),
        Some(raw) => {
            anyhow::bail!("autodone «{raw}» не разобран: жди on | off (без значения — on)")
        }
    };
    // канон-форма (не эхо ввода): ядро апсертит mSelect-опцию по
    // точному content — «ревью плана» mint'нул бы дубль «Ревью плана»
    let status_canonical = status.as_deref().map(syt::validate_status).transpose()?;
    if let Some(raw) = planned.as_deref().filter(|s| !s.is_empty()) {
        syt::parse_planned(raw)?;
    }
    if let Some(raw) = title.as_deref() {
        syt::edit_title(sy, &board, &task, raw).await?;
    }
    syt::edit(
        sy,
        &board,
        &task,
        &syt::CellUpdates {
            status: status_canonical.as_deref(),
            branch: branch.as_deref(),
            pr: pr.as_deref(),
            ty: ty_canonical.as_deref(),
            priority: priority_canonical.as_deref(),
            autodone: autodone_value,
        },
    )
    .await?;
    // серединные сбои после базового edit: колонки уже записаны —
    // ошибка называет применённое и рецепт доставки (не частичные
    // println-подтверждения: финальное состояние печатается в конце)
    if let Some(raw) = planned.as_deref() {
        syt::set_planned(sy, &board, &task, raw)
            .await
            .with_context(|| {
                format!(
                    "колонки строки записаны, но «Запланировано» не записано — \
                         доставь: task edit {} --planned \"{raw}\"",
                    task.item_id
                )
            })?;
    }
    if let Some(expr) = schedule.as_deref() {
        // пустая строка = снятие атрибута (ядро удаляет ключ из IAL);
        // валидация — только для непустого значения: контракт «"" — снять»
        // не должен упираться в отказ грамматики
        if !expr.is_empty() {
            crate::schedule::validate_schedule(expr)?;
        }
        let body = syt::task_body_doc(sy, &board, &task)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "строка «{}» без тела (detached) — расписанию некуда записаться",
                    task.title
                )
            })?;
        sy.set_block_attr(&body, "custom-schedule", expr)
            .await
            .with_context(|| {
                format!(
                    "колонки строки записаны, но атрибут custom-schedule не записан — \
                         доставь: task edit {} --schedule \"{expr}\"",
                    task.item_id
                )
            })?;
        // колонка «Расписание» — отображение attr: пишется синхронно
        // в единственной точке записи расписания (снятие чистит клетку)
        syt::sync_schedule_cell(sy, &board, &task.item_id, expr)
            .await
            .with_context(|| {
                format!(
                    "атрибут записан, но клетка «{}» не записана — доставь: \
                             task edit {} --schedule \"{expr}\"",
                    syt::SCHEDULE_CELL,
                    task.item_id
                )
            })?;
    }
    if let Some(name) = agent.as_deref() {
        // пустая строка = снятие атрибута (мастер файрится
        // изолированными спавнами); валидация — только непустого
        if !name.is_empty() {
            syt::validate_agent_name(name)?;
        }
        let body = syt::task_body_doc(sy, &board, &task)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "строка «{}» без тела (detached) — агенту некуда записаться",
                    task.title
                )
            })?;
        sy.set_block_attr(&body, "custom-agent", name)
            .await
            .with_context(|| {
                format!(
                    "колонки строки записаны, но атрибут custom-agent не записан — \
                         доставь: task edit {} --agent \"{name}\"",
                    task.item_id
                )
            })?;
    }
    // контракт ответов: ответ = финальное состояние строки (шапка,
    // перечитанная с ядра, без тела — edit тело не меняет); сбой
    // перечитывания → ADR-067 warning, ретрай edit безопасен
    print_final_state(sy, &board, &task).await;
    Ok(())
}

pub(super) async fn run_claim(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Claim { id, session } = command else {
        unreachable!("task_run роутит Claim в edit::run_claim");
    };
    let session = match session {
                Some(sid) => sid,
                None => std::env::var("OPENCODE_SESSION_ID").map_err(|_| anyhow!(
                    "sessionID не задан: нет --session и нет OPENCODE_SESSION_ID в env — заявись из сессии агента или передай id явно"
                ))?,
            };
    let (board, task) = resolve_task(sy, project, &id).await?;
    syt::claim(sy, &board, &task, &session).await?;
    // контракт ответов: шапка перечитанной строки («Сессия dev» видна)
    print_final_state(sy, &board, &task).await;
    Ok(())
}

pub(super) async fn run_close(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Close { id, note } = command else {
        unreachable!("task_run роутит Close в edit::run_close");
    };
    let (board, task) = resolve_task(sy, project, &id).await?;
    syt::close(sy, &board, &task, note.as_deref()).await?;
    // контракт ответов: шапка перечитанной строки (статус Done виден)
    print_final_state(sy, &board, &task).await;
    Ok(())
}
