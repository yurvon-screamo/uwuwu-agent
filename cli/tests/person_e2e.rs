//! E2E-контракт `person` на живом ядре SiYuan: show на отсутствующем доке —
//! честная ошибка с подсказкой создания; append создаёт док со скелетом
//! (create-on-missing) и дописывает факты; replace перезаписывает контент
//! на месте с тем же docId; дубль hpath — громкая ошибка. Канонический путь
//! в проде — Experience:/personal/PERSON; здесь e2e ходит в zz-e2e-док через
//! env-шов (UWUWU_PERSON_NOTEBOOK/UWUWU_PERSON_HPATH), читатель — pub-API
//! siyuan_person (тот же слой, что вызывает dispatch).
//!
//! Запуск: UWUWU_E2E=1 cargo test (без живого ядра тесты сразу зелёные).
//! Доки создаются под /personal/zz-e2e-person и удаляются в конце каждого
//! теста (в начале — зачистка хвостов упавшего прогона: индексная задержка
//! делает вторичную зачистку надёжнее). Ядро — управляемая зависимость:
//! createDocWithMd валит индекс под параллельной записью — серийно.

use anyhow::Result;
use serial_test::serial;
use uwuwu_cli::siyuan::SiYuan;
use uwuwu_cli::siyuan_person::{self, PersonTarget};

const E2E_NOTEBOOK: &str = "Experience";
const E2E_HPATH: &str = "/personal/zz-e2e-person";

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

fn e2e_target() -> PersonTarget {
    std::env::set_var("UWUWU_PERSON_NOTEBOOK", E2E_NOTEBOOK);
    std::env::set_var("UWUWU_PERSON_HPATH", E2E_HPATH);
    siyuan_person::person_target()
}

async fn cleanup(sy: &SiYuan) -> Result<()> {
    let nb = sy.ensure_notebook(E2E_NOTEBOOK).await?;
    for id in sy.doc_ids_by_hpath(&nb, E2E_HPATH).await? {
        let _ = sy.remove_doc(&id).await;
    }
    // индекс ядра перестраивается асинхронно: дока может быть видна запросам
    // после remove — ждём исчезновения, иначе следующий тест создаст дубль
    for _ in 0..10 {
        if sy.doc_ids_by_hpath(&nb, E2E_HPATH).await?.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok(())
}

#[tokio::test]
#[serial]
async fn show_on_missing_doc_is_honest_error() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let target = e2e_target();
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    let err = siyuan_person::show(&sy, &target).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("персона не заведена"),
        "неожиданная ошибка: {msg}"
    );
    assert!(
        msg.contains("person append"),
        "ошибка должна звать создание: {msg}"
    );
    cleanup(&sy).await
}

#[tokio::test]
#[serial]
async fn append_creates_doc_with_skeleton_then_appends() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let target = e2e_target();
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    // первый append: create-on-missing, док = скелет + текст
    let doc_id = siyuan_person::append(&sy, &target, "предпочитает краткие ответы").await?;
    let md = siyuan_person::show(&sy, &target).await?;
    assert!(md.contains("Границы приватности"), "скелет отсутствует");
    assert!(md.contains("Кто владелец"), "секции скелета отсутствуют");
    assert!(
        md.contains("предпочитает краткие ответы"),
        "текст первого append потерян"
    );

    // второй append: в существующий док, ничего не теряем
    siyuan_person::append(
        &sy,
        &target,
        "\n## Анти-паттерны (не делать)\n\nне молчать при падении тестов",
    )
    .await?;
    let md = siyuan_person::show(&sy, &target).await?;
    assert!(md.contains("предпочитает краткие ответы"));
    assert!(md.contains("не молчать при падении тестов"));

    // docId стабилен между append'ами (никаких remove+recreate)
    let doc_id_after = siyuan_person::resolve(&sy, &target)
        .await?
        .expect("док должен жить");
    assert_eq!(doc_id, doc_id_after, "append пересоздал док");
    cleanup(&sy).await
}

#[tokio::test]
#[serial]
async fn replace_rewrites_in_place_keeps_doc_id() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let target = e2e_target();
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    siyuan_person::append(&sy, &target, "черновой факт").await?;
    let before = siyuan_person::resolve(&sy, &target)
        .await?
        .expect("док должен жить");

    siyuan_person::replace(
        &sy,
        &target,
        "## Кто владелец\n\nЮрий; любит плотные ответы без воды",
    )
    .await?;
    let md = siyuan_person::show(&sy, &target).await?;
    assert!(
        !md.contains("черновой факт"),
        "replace не стёр старый контент"
    );
    assert!(md.contains("без воды"), "replace не записал новый контент");
    assert!(
        !md.contains("Границы приватности"),
        "replace должен писать ровно данный контент"
    );

    let after = siyuan_person::resolve(&sy, &target)
        .await?
        .expect("док должен жить");
    assert_eq!(before, after, "replace пересоздал док");
    cleanup(&sy).await
}

#[tokio::test]
#[serial]
async fn replace_creates_when_missing() -> Result<()> {
    if !e2e_enabled() {
        return Ok(());
    }
    let target = e2e_target();
    let sy = SiYuan::from_env()?;
    cleanup(&sy).await?;

    siyuan_person::replace(&sy, &target, "# своя шапка\n\nполный контент от владельца").await?;
    let md = siyuan_person::show(&sy, &target).await?;
    assert!(md.contains("полный контент от владельца"));
    assert!(
        !md.contains("Границы приватности"),
        "replace не должен подмешивать скелет"
    );
    cleanup(&sy).await
}
