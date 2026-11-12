//! E2E-контракт create (20261109101129): непустой контент --stdin — тело
//! целиком как есть («передал контент — тело твоё»), скелет не вкатывается;
//! титульный H1 стрипается паритетом с task body (createDocWithMd оставил бы
//! его heading-блоком — дубль к заголовку дока), чужой H1 остаётся. Главный
//! путь на живом ядре SiYuan через pub-API syt::create — тот же слой, что
//! вызывает CLI-dispatch.
//!
//! Второй риг — начальный статус: --status Todo минтит строку сразу в Todo
//! (крон-ран-карточки, задача 20261112075035-6f8tz63) — клетку ядра читает
//! read_rows, draftLoop триажа strict-матчем Draft ран не отбирает.
//!
//! Запуск: UWUWU_E2E=1 cargo test (без живого ядра тесты сразу зелёные).
//! Задачи создаются на прод-доске «Задачи uwuwu» под zz-тайтлом (Draft /
//! машинное Todo) и сносятся в конце теста; в начале — зачистка хвостов
//! упавшего прогона (тот же компромисс, что zz-e2e-доки person_e2e в
//! прод-ноутбуке Experience).

use anyhow::Result;
use serial_test::serial;
use serde_json::json;
use uwuwu_cli::siyuan::SiYuan;
use uwuwu_cli::siyuan_task as syt;

const PROJECT: &str = "uwuwu";
const TITLE: &str = "zz-e2e create-stdin: контент вместо скелета";

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

/// Снос zz-строк и их тел с доски проекта: item_remove + remove_doc
/// (механика task rm без guard'а — тест сам отвечает за свои строки).
/// Индексная задержка ядра: свежая строка может не устоять в av-блоке к
/// моменту item_remove (code=0, строка видна) — после сноса ждём исчезновения
/// из read_rows, иначе хвост доживает до зачистки начала следующего прогона
/// (тот же паттерн, что у person_e2e).
async fn cleanup(sy: &SiYuan) -> Result<()> {
    let board = syt::find_board(sy, PROJECT).await?;
    for _ in 0..10 {
        let rows = syt::read_rows(sy, &board).await?;
        let zz: Vec<_> = rows
            .iter()
            .filter(|r| r.title.starts_with("zz-e2e create-"))
            .collect();
        if zz.is_empty() {
            return Ok(());
        }
        for row in &zz {
            sy.mcp_tool(
                "database",
                json!({ "action": "item_remove", "id": board.av_id, "itemIDs": [row.item_id] }),
            )
            .await?;
            if let Some(doc) = syt::task_body_doc(sy, &board, row).await? {
                let _ = sy.remove_doc(&doc).await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok(())
}

#[tokio::test]
#[serial]
async fn create_with_content_writes_content_as_body_on_live_kernel() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    let content = format!(
        "# {TITLE}\n\n## Постановка\n\nсвоё тело e2e — скелет не нужен\n\n# Чужой Раздел\n\nостаётся\n"
    );
    let (_item_id, body_doc) = syt::create(
        &sy,
        PROJECT,
        TITLE,
        syt::DEFAULT_TASK_TYPE,
        &content,
        "Draft",
    )
    .await?;

    let md = sy.export_md_by_id(&body_doc).await?;
    assert!(
        md.contains("## Постановка\n\nсвоё тело e2e — скелет не нужен"),
        "контент записан как есть: {md}"
    );
    assert!(
        !md.contains("<срезы/шаги") && !md.contains("<исполняемое"),
        "плейсхолдеров скелета в теле нет: {md}"
    );
    assert!(md.contains("# Чужой Раздел"), "чужой H1 остаётся: {md}");
    assert_eq!(
        md.matches(&format!("# {TITLE}")).count(),
        1,
        "титульный H1 стрипается: в теле ровно штатный заголовок дока, дубля нет: {md}"
    );

    cleanup(&sy).await?;
    Ok(())
}

/// E2E-контракт create --status (крон-ран-карточки, задача
/// 20261112075035-6f8tz63): начальный статус Todo записан в клетку строки
/// на живом ядре — draftLoop триажа (strict-матч Draft) ран не видит.
/// Через syt::create — тот же слой, что CLI-dispatch после валидации флага
/// (саму валидацию покрывают мок-тесты dispatch и юниты validate_create_status).
#[tokio::test]
#[serial]
async fn create_with_status_todo_mints_row_in_todo_on_live_kernel() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    const TODO_TITLE: &str = "zz-e2e create-status: ран минтится Todo";
    let (item_id, _body_doc) =
        syt::create(&sy, PROJECT, TODO_TITLE, syt::DEFAULT_TASK_TYPE, "", "Todo").await?;

    let board = syt::find_board(&sy, PROJECT).await?;
    let rows = syt::read_rows(&sy, &board).await?;
    let row = rows
        .iter()
        .find(|r| r.item_id == item_id)
        .expect("созданная строка видна в read_rows");
    assert_eq!(
        row.status, "Todo",
        "начальный статус Todo в клетке ядра (не Draft): {}",
        row.status
    );

    cleanup(&sy).await?;
    Ok(())
}
