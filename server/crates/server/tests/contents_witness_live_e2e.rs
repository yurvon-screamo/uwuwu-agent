//! Живая проба #1653: повторная запись и append сразу после commit'а.
//!
//! Риг поднимается и сносится самим тестом через REST (private,
//! auto_init, ветка из default_branch; ADR-110 — HTTP-моков нет).
//! Сервер — бинарь ветки, прод-драйвер github, писатель GitWikiWriter.
//! Сценарий проверяет, что ответ тула и содержимое дока не зависят от
//! устаревшего Contents-чтения: шапка переживает replace, append
//! дописывает строку, а не создаёт файл заново.
//!
//! Запуск:
//!   UWUWU_E2E=1 cargo test -p uwuwu-server --test contents_witness_live_e2e -- --nocapture

mod common;

use common::github_rig::Rig;
use common::github_stand::{mcp_call, spawn};
use serde_json::json;

#[tokio::test]
async fn write_then_write_and_append_after_commit_keep_doc_contract() {
    let enabled = std::env::var("UWUWU_E2E").ok().as_deref() == Some("1");
    let Some(token) = std::env::var("GH_TOKEN").ok().filter(|t| !t.is_empty()) else {
        println!("e2e выключен: нужен GH_TOKEN");
        return;
    };
    if !enabled {
        println!("e2e выключен: UWUWU_E2E=1 для живой пробы");
        return;
    }
    let http = reqwest::Client::new();
    let rig = Rig::create(http.clone(), &token).await;
    let dir = std::env::temp_dir().join(format!(
        "uwuwu-witness-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis()
    ));
    std::fs::create_dir_all(&dir).expect("rig dir");
    let stand = spawn(&dir, &rig.full, &token).await;

    let slug = format!("Experience/contents-witness-{}", std::process::id());
    let path = format!("docs/{slug}.md");
    let header = "---\ntitle: witness-original\ndate: 2026-01-01T00:00:00+09:00\nlastmod: 2026-01-01T00:00:00+09:00\n---\n\nfirst body\n";

    let (err, text) = mcp_call(
        &http,
        &stand,
        "docs_write",
        json!({"slug": slug, "content": header}),
    )
    .await;
    assert!(!err, "первая запись: {text}");
    rig.wait_blob(&path, "first body").await;

    // Вторая запись без fm сразу после commit'а: шапка обязана выжить.
    let (err, text) = mcp_call(
        &http,
        &stand,
        "docs_write",
        json!({"slug": slug, "content": "second body\n"}),
    )
    .await;
    assert!(!err, "вторая запись: {text}");
    assert!(text.contains("carried over"), "ожидался Carried: {text}");

    // Append сразу после commit'а: строка дописывается, файл не создаётся.
    let (err, text) = mcp_call(
        &http,
        &stand,
        "docs_append",
        json!({"slug": slug, "line": "appended line"}),
    )
    .await;
    assert!(!err, "append: {text}");
    assert!(
        text.contains("appended to"),
        "append не должен отказывать/создавать: {text}"
    );

    let content = rig.wait_blob(&path, "appended line").await;
    assert!(
        content.contains("title: witness-original"),
        "шапка потеряна: {content}"
    );
    assert!(
        content.contains("second body\nappended line\n"),
        "тело или строка не на месте: {content}"
    );
}
