//! Реестр долгосрочных целей проекта: доска «Цели <proj>» (SiYuan database).
//! Доску заводит владелец руками в UI — одноразовая настройка, CLI её не
//! создаёт и громко отказывается без неё или без Status-колонки. Строки
//! bound к телам-саб-докам («как у задач»), статусы канонические:
//! todo / in progress / done. Связей с задачами и автоматизаций нет.

use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{anyhow, bail, Result};

/// Канонические статусы канбана целей.
pub const GOAL_STATUSES: [&str; 3] = ["todo", "in progress", "done"];

/// Доска целей: док /<project>/Цели <project> + database-блок с колонкой
/// Status. Дока нет → громкий отказ с инструкцией (заведение — одноразовая
/// работа владельца в UI); инфра-сбой ядра или битая доска → проброс ошибки
/// (маскировать нельзя — find_board_titled_opt различает таксономию).
pub async fn find_goal_board(sy: &SiYuan, project: &str) -> Result<syt::SyBoard> {
    let title = format!("Цели {project}");
    let board = syt::find_board_titled_opt(sy, project, &title)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "доски «{title}» нет — заводит владелец руками в UI: док \
             /Projects/{project}/{title} с database-блоком и колонкой Status \
             (mSelect: todo / in progress / done); Title есть у любой database"
            )
        })?;
    if syt::board_keys(sy, &board.av_id)
        .await?
        .iter()
        .all(|(name, _)| name != "Status")
    {
        bail!(
            "на доске «{title}» нет колонки Status (mSelect: todo / in progress / done) — добавь в UI"
        );
    }
    Ok(board)
}

/// Канонизация статуса цели: lowercase, underscore/dash → пробел. Вне канона
/// — отказ с перечнем: ядро молча создаёт недостающие опции mSelect, и
/// мусорная опция поселилась бы на владельческой доске навсегда.
pub fn canonical_status(raw: &str) -> Result<String> {
    let normalized = raw.trim().to_lowercase().replace(['_', '-'], " ");
    GOAL_STATUSES
        .into_iter()
        .find(|canon| *canon == normalized)
        .map(String::from)
        .ok_or_else(|| {
            anyhow!(
                "неизвестный статус «{raw}» — канон целей: {}",
                GOAL_STATUSES.join(" | ")
            )
        })
}

/// Костяк тела цели: свободное описание под «Суть» (контекст из --stdin).
pub fn goal_body_template(ctx: &str) -> String {
    format!("## Суть\n{ctx}\n")
}

/// Создание цели: bound-строка todo-класса на доске целей (общий
/// create_row_with_body: идемпотентность по тайтлу, перепривязка сироты,
/// verify вставки ×1).
pub async fn create(
    sy: &SiYuan,
    project: &str,
    title: &str,
    status: &str,
    ctx: &str,
) -> Result<(String, String)> {
    let board = find_goal_board(sy, project).await?;
    syt::create_row_with_body(
        sy,
        &board,
        title,
        &canonical_status(status)?,
        &goal_body_template(ctx),
        None,
        // прод-пейсинг verify-ретрая; инъекция мгновенного — при переводе
        // goal-тестов с их Kernel-мока на общий MockKernel
        &syt::RowInsertPacing::production(),
    )
    .await
}
