//! E2E-контракт `read` на живом ядре SiYuan: слаг project/doc и topic/doc,
//! одиночный слаг Daily, развод одноимённых доков по папке проекта,
//! перечень кандидатов для неоднозначного заголовка, внятная ошибка для
//! несуществующего слага. Канонический путь чтения доков из AGENTS.md.
//!
//! Запуск: UWUWU_E2E=1 cargo test (без живого ядра тесты сразу зелёные).
//! Доки создаются через Kernel API под префиксами zz-e2e-read-… и удаляются
//! в конце каждого теста (в начале — зачистка хвостов упавшего прогона:
//! удаление с index-задержкой делает вторичную зачистку надёжнее).
//! Родителей вложенных путей создаём по цепочке: createDocWithMd не
//! создаёт промежуточные доки.

use anyhow::Result;
use serial_test::serial;
use uwuwu_cli::cmd::read_markdown;
use uwuwu_cli::siyuan::SiYuan;

const PROJECTS: &str = "Projects";
const EXPERIENCE: &str = "Experience";
const DAILY: &str = "Daily";

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

// Живое ядро — управляемая зависимость: createDocWithMd валит дерево индексов
// и под параллельной записью отвечает «metadata is not ready» — гоняем серийно.

/// Доки прошлого неудавшегося прогона с этим hpath-префиксом.
async fn remove_stale(sy: &SiYuan, hpath_prefix: &str) -> Result<()> {
    let stmt = format!(
        "select id from blocks where type='d' and hpath like '{}%'",
        hpath_prefix.replace('\'', "''")
    );
    for row in sy.sql(&stmt).await? {
        if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
            let _ = sy.remove_doc(id).await;
        }
    }
    Ok(())
}

/// createDocWithMd + ожидание индекса. Ретраим «metadata is not ready»:
/// под чередой созданий/удалений ядро долго перестраивает метаданные —
/// это не предмет теста (read-резолв), изолируем его от чурна ядра.
async fn create_doc(sy: &SiYuan, notebook: &str, hpath: &str, markdown: &str) -> Result<()> {
    let notebook_id = sy.ensure_notebook(notebook).await?;
    for _ in 0..5 {
        match sy.create_doc_md(&notebook_id, hpath, markdown).await {
            Ok(_) => return wait_indexed(sy, hpath).await,
            Err(e) if e.to_string().contains("metadata is not ready") => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("createDocWithMd {hpath}: ядро не готово после 5 попыток")
}

/// Ядро индексирует блоки асинхронно и не по порядку: после createDocWithMd
/// док виден в SQL с задержкой — ждём появления (ограниченно).
async fn wait_indexed(sy: &SiYuan, hpath: &str) -> Result<()> {
    let stmt = format!(
        "select id from blocks where type='d' and hpath = '{}'",
        hpath.replace('\'', "''")
    );
    for _ in 0..20 {
        if !sy.sql(&stmt).await?.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("док {hpath} не проиндексирован за 10с")
}

#[tokio::test]
#[serial]
async fn project_slug_resolves_doc_by_full_hpath() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    remove_stale(&sy, "/zz-e2e-read-p").await.unwrap();
    let result = project_slug_scenario(&sy).await;
    remove_stale(&sy, "/zz-e2e-read-p").await.unwrap();
    result.unwrap();
}

async fn project_slug_scenario(sy: &SiYuan) -> Result<()> {
    // вложенный путь: цепочка root → тема → док (родители вручную)
    create_doc(sy, PROJECTS, "/zz-e2e-read-p", "# zz-e2e-read-p\n").await?;
    create_doc(sy, PROJECTS, "/zz-e2e-read-p/Тема", "# Тема\n").await?;
    create_doc(
        sy,
        PROJECTS,
        "/zz-e2e-read-p/Тема/Дочерний док",
        "# Дочерний док\n\nпроектный маркер e2e",
    )
    .await?;
    // заголовок с апострофом: SQL-литерал не должен ломаться
    create_doc(
        sy,
        PROJECTS,
        "/zz-e2e-read-p/Док с 'апострофом'",
        "# апострофный маркер e2e",
    )
    .await?;

    let md = read_markdown("zz-e2e-read-p/Тема/Дочерний док").await?;
    assert!(md.contains("проектный маркер e2e"), "nested slug: {md}");

    let md = read_markdown("zz-e2e-read-p/Док с 'апострофом'").await?;
    assert!(
        md.contains("апострофный маркер e2e"),
        "apostrophe slug: {md}"
    );
    Ok(())
}

#[tokio::test]
#[serial]
async fn experience_slug_resolves_doc_by_full_hpath() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    remove_stale(&sy, "/zz-e2e-read-x").await.unwrap();
    let result = experience_slug_scenario(&sy).await;
    remove_stale(&sy, "/zz-e2e-read-x").await.unwrap();
    result.unwrap();
}

async fn experience_slug_scenario(sy: &SiYuan) -> Result<()> {
    create_doc(sy, EXPERIENCE, "/zz-e2e-read-x", "# zz-e2e-read-x\n").await?;
    create_doc(
        sy,
        EXPERIENCE,
        "/zz-e2e-read-x/Заметка",
        "# Заметка\n\nопытный маркер e2e",
    )
    .await?;

    let md = read_markdown("zz-e2e-read-x/Заметка").await?;
    assert!(md.contains("опытный маркер e2e"), "experience slug: {md}");
    Ok(())
}

#[tokio::test]
#[serial]
async fn daily_single_slug_resolves_doc() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    remove_stale(&sy, "/zz-e2e-read-daily").await.unwrap();
    let result = daily_slug_scenario(&sy).await;
    remove_stale(&sy, "/zz-e2e-read-daily").await.unwrap();
    result.unwrap();
}

async fn daily_slug_scenario(sy: &SiYuan) -> Result<()> {
    create_doc(
        sy,
        DAILY,
        "/zz-e2e-read-daily",
        "# zz-e2e-read-daily\n\ndaily маркер e2e",
    )
    .await?;

    let md = read_markdown("zz-e2e-read-daily").await?;
    assert!(md.contains("daily маркер e2e"), "daily single slug: {md}");
    Ok(())
}

#[tokio::test]
#[serial]
async fn same_title_docs_disambiguated_by_project_prefix() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    remove_stale(&sy, "/zz-e2e-read-amb").await.unwrap();
    let result = disambiguation_scenario(&sy).await;
    remove_stale(&sy, "/zz-e2e-read-amb").await.unwrap();
    result.unwrap();
}

async fn disambiguation_scenario(sy: &SiYuan) -> Result<()> {
    // цепочка родителей: ядро не создаёт промежуточные доки само
    create_doc(sy, PROJECTS, "/zz-e2e-read-amb", "# zz-e2e-read-amb\n").await?;
    create_doc(sy, PROJECTS, "/zz-e2e-read-amb/проект-а", "# проект-а\n").await?;
    create_doc(
        sy,
        PROJECTS,
        "/zz-e2e-read-amb/проект-а/Общий док",
        "# Общий док\n\nмаркер проекта а",
    )
    .await?;
    create_doc(sy, PROJECTS, "/zz-e2e-read-amb/проект-б", "# проект-б\n").await?;
    create_doc(
        sy,
        PROJECTS,
        "/zz-e2e-read-amb/проект-б/Общий док",
        "# Общий док\n\nмаркер проекта б",
    )
    .await?;

    // полный слаг разводит одноимённые доки по папке проекта
    let md = read_markdown("zz-e2e-read-amb/проект-а/Общий док").await?;
    assert!(md.contains("маркер проекта а"), "project a slug: {md}");
    let md = read_markdown("zz-e2e-read-amb/проект-б/Общий док").await?;
    assert!(md.contains("маркер проекта б"), "project b slug: {md}");

    // заголовок без префикса неоднозначен → перечень кандидатов с docId
    let err = read_markdown("Общий док").await.unwrap_err().to_string();
    assert!(err.contains("неоднозначен"), "ambiguous title: {err}");
    assert!(err.contains("/zz-e2e-read-amb/проект-а/Общий док"), "{err}");
    assert!(err.contains("/zz-e2e-read-amb/проект-б/Общий док"), "{err}");
    assert!(err.contains("2026"), "candidate docId in error: {err}");

    // несуществующий слаг → «не найден», а не маскированная ошибка API
    let err = read_markdown("zz-e2e-read-amb/проект-а/Нет такого дока")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("не найден"), "missing slug: {err}");
    Ok(())
}
