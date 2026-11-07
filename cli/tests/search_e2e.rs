//! E2E-контракт doc-секции search на живом ядре SiYuan: док с маркером в
//! заголовке резолвится секцией «Доки (по hpath)» — lowercase-запрос
//! (SQLite LIKE для кириллицы регистрозависим, матчит Rust), AND-токены
//! многословного тайтла, фетч doc_titles с явным LIMIT.
//! Родитель doc-секции read_e2e.rs — та же дисциплина создания/зачистки.
//!
//! Запуск: UWUWU_E2E=1 cargo test (без живого ядра тесты сразу зелёные).
//! Доки создаются под префиксом zz-e2e-search- и удаляются в конце каждого
//! теста (в начале — зачистка хвостов упавшего прогона).

use anyhow::Result;
use serial_test::serial;
use uwuwu_cli::cmd::search_lines;
use uwuwu_cli::siyuan::SiYuan;

const PROJECTS: &str = "Projects";
const MARKER_PREFIX: &str = "/zz-e2e-search";

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

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

/// createDocWithMd + ожидание индекса (ретрай «metadata is not ready» —
/// ядро перестраивает метаданные под чередой созданий, это не предмет теста).
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

/// Ядро индексирует блоки асинхронно: ждём появления дока в SQL.
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
async fn doc_section_resolves_lowercase_cyrillic_multitoken_title() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    remove_stale(&sy, MARKER_PREFIX).await.unwrap();
    let result = marker_title_scenario(&sy).await;
    remove_stale(&sy, MARKER_PREFIX).await.unwrap();
    result.unwrap();
}

async fn marker_title_scenario(sy: &SiYuan) -> Result<()> {
    let marker = format!(
        "{MARKER_PREFIX}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
    );
    let hpath = format!("{marker} Доменный Ствол Поиска");
    create_doc(sy, PROJECTS, &hpath, "# маркерный док e2e\n").await?;

    // production-путь stdout search (голова + полнотекст + doc-секция);
    // lowercase-запрос с AND-токенами маркера и тайтла
    let query = format!("{} доменный ствол", marker.trim_start_matches('/'));
    let lines = search_lines(sy, &query, 10).await?;

    let doc_section: Vec<&String> = lines
        .iter()
        .skip_while(|l| *l != "Доки (по hpath):")
        .collect();
    assert!(
        !doc_section.is_empty(),
        "doc-секция обязана присутствовать в выдаче: {lines:?}"
    );
    assert!(
        doc_section.iter().any(|l| l.contains(&hpath)),
        "lowercase AND-запрос обязан резолвить созданный тайтл: {doc_section:?}"
    );
    Ok(())
}

#[tokio::test]
#[serial]
async fn doc_section_empty_query_is_omitted_on_live_kernel() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().unwrap();
    let lines = search_lines(&sy, "   ", 10).await.unwrap();
    assert!(!lines.iter().any(|l| l == "Доки (по hpath):"));
}
