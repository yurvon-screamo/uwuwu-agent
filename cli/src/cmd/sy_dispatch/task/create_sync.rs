//! Крепления к созданной задаче: расписание (атрибут + клетка) и агент
//! (атрибут custom-agent). Сбой после create не теряет задачу — ошибки
//! называют рецепт доставки (task edit …).

use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{Context, Result};

/// Крепление расписания к созданной задаче: атрибут + клетка «Расписание».
/// Сбой после create не теряет задачу — ошибка называет рецепт доставки.
pub(super) async fn sync_schedule_on_create(
    sy: &SiYuan,
    project: &str,
    item: &str,
    body: &str,
    expr: &str,
) -> Result<()> {
    sy.set_block_attr(body, "custom-schedule", expr)
        .await
        .with_context(|| {
            format!(
                "задача создана (строка {item}, тело {body}), но расписание \
                 не записано — доставь: task edit {item} --schedule \"{expr}\""
            )
        })?;
    // колонка «Расписание» — синхронно с attr (доску create не
    // возвращает; повторный find — две read-пробы, только при --schedule)
    let board = syt::find_board(sy, project).await.with_context(|| {
        format!(
            "задача создана (строка {item}), но клетку «{}» доставь: \
             task edit {item} --schedule \"{expr}\"",
            syt::SCHEDULE_CELL
        )
    })?;
    syt::sync_schedule_cell(sy, &board, item, expr).await?;
    Ok(())
}

/// Крепление агента к созданной задаче (атрибут custom-agent). Сбой после
/// create не теряет задачу — ошибка называет рецепт доставки.
pub(super) async fn sync_agent_on_create(
    sy: &SiYuan,
    item: &str,
    body: &str,
    name: &str,
) -> Result<()> {
    sy.set_block_attr(body, "custom-agent", name)
        .await
        .with_context(|| {
            format!(
                "задача создана (строка {item}, тело {body}), но агент \
                 не записан — доставь: task edit {item} --agent \"{name}\""
            )
        })?;
    Ok(())
}
