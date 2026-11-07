//! Тесты search: golden полнотекстовой секции (байт-контракт парсинга
//! фабричных агентов, регрессия 20261104072044), матчер doc-секции
//! (кириллица-case, AND-токены, ранжирование, лимит), фетч doc_titles
//! (явный LIMIT — без него ядро капает SQL на 64 строки) и порядок
//! секций сборки stdout.

use super::sy_dispatch::{doc_section_lines, doc_section_result_lines, fulltext_lines, search_lines};
use crate::siyuan::SiYuan;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// ——— полнотекстовая секция: golden формата ———

#[test]
fn fulltext_section_keeps_byte_format_per_row() {
    // Arrange
    let rows = vec![json!({
        "hpath": "/uwuwu/ADR/ADR-006: x",
        "content": "строка1\nстрока2",
    })];

    // Act
    let lines = fulltext_lines(&rows);

    // Assert
    assert_eq!(
        lines,
        vec![
            "1. /uwuwu/ADR/ADR-006: x".to_string(),
            "    строка1 строка2".to_string(),
        ]
    );
}

#[test]
fn fulltext_section_empty_result_reports_no_hits() {
    // Act + Assert
    assert_eq!(fulltext_lines(&[]), vec!["ничего не найдено".to_string()]);
}

// ——— doc-секция: матчер ———

#[test]
fn doc_section_matches_lowercase_cyrillic_title() {
    // SQLite LIKE для кириллицы регистрозависим (проба 07.11) —
    // lowercase-запрос обязан находиться Rust-матчером.
    // Arrange
    let hpaths = vec!["/uwuwu/Стек и архитектура".to_string()];

    // Act
    let lines = doc_section_lines("стек и архитектура", &hpaths, 10);

    // Assert
    assert_eq!(
        lines,
        vec![
            "Доки (по hpath):".to_string(),
            "1. /uwuwu/Стек и архитектура".to_string(),
        ]
    );
}

#[test]
fn doc_section_ranks_exact_substring_above_token_matches() {
    // Arrange
    let hpaths = vec![
        "/uwuwu/ADR/ADR-006: Доверие ok:false+retryable — whitelist кодов, шлюз не форкаем"
            .to_string(),
        "/uwuwu/Задачи uwuwu/Доверие шлюз — сводка".to_string(),
    ];

    // Act
    let lines = doc_section_lines("Доверие шлюз", &hpaths, 10);

    // Assert: точная подстрока «доверие шлюз» — первой, токенный матч — следом
    assert_eq!(lines[0], "Доки (по hpath):");
    assert_eq!(lines[1], "1. /uwuwu/Задачи uwuwu/Доверие шлюз — сводка");
    assert_eq!(
        lines[2],
        "2. /uwuwu/ADR/ADR-006: Доверие ok:false+retryable — whitelist кодов, шлюз не форкаем"
    );
}

#[test]
fn doc_section_requires_every_token_in_hpath() {
    // Arrange
    let hpaths = vec!["/uwuwu/Стек и архитектура".to_string()];

    // Act
    let lines = doc_section_lines("стек orig", &hpaths, 10);

    // Assert: токен «orig» в hpath нет — AND-матч пуст
    assert!(lines.is_empty());
}

#[test]
fn doc_section_omitted_for_whitespace_only_query() {
    // Arrange
    let hpaths = vec!["/uwuwu/Стек".to_string()];

    // Act + Assert
    assert!(doc_section_lines("   ", &hpaths, 10).is_empty());
}

#[test]
fn doc_section_takes_top_n_per_method_limit() {
    // Arrange
    let hpaths: Vec<String> = (0..3).map(|i| format!("/p/doc{i}")).collect();

    // Act
    let lines = doc_section_lines("doc", &hpaths, 2);

    // Assert
    assert_eq!(lines.len(), 3, "заголовок + limit строк");
    assert!(lines[2].starts_with("2. "), "{lines:?}");
}

#[test]
fn doc_section_colliding_adr_numbers_show_project_in_hpath() {
    // Реплика приёмки: ADR-006 коллирует между проектами — полный hpath
    // в каждой строке дискриминирует «нашёлся наш».
    // Arrange
    let hpaths = vec![
        "/uwuwu/ADR/ADR-006: Доверие ok:false+retryable".to_string(),
        "/true-washing/ADR/ADR-006: Дописывание плагинов SAAS".to_string(),
        "/origa/ADR/ADR-006: Phrase Dataset Filtering".to_string(),
    ];

    // Act
    let lines = doc_section_lines("ADR-006", &hpaths, 10);

    // Assert
    assert_eq!(
        lines,
        vec![
            "Доки (по hpath):".to_string(),
            "1. /uwuwu/ADR/ADR-006: Доверие ok:false+retryable".to_string(),
            "2. /true-washing/ADR/ADR-006: Дописывание плагинов SAAS".to_string(),
            "3. /origa/ADR/ADR-006: Phrase Dataset Filtering".to_string(),
        ]
    );
}

// ——— doc-секция: отказ фетча — статус внутри секции ———

#[test]
fn doc_section_fetch_failure_becomes_status_inside_section() {
    // Act
    let lines = doc_section_result_lines("q", 10, Err(anyhow::anyhow!("sql сломан")));

    // Assert: команда не падает, статус виден в stdout
    assert_eq!(
        lines,
        vec![
            "Доки (по hpath):".to_string(),
            "kernel sql недоступен: sql сломан".to_string(),
        ]
    );
}

// ——— doc_titles: фетч с явным LIMIT ———

#[tokio::test]
async fn doc_titles_fetches_hpaths_with_explicit_limit() {
    // Arrange
    let server = MockServer::start().await;
    let stmts = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&stmts);
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if let Some(stmt) = body.get("stmt").and_then(|s| s.as_str()) {
                capture.lock().unwrap().push(stmt.to_string());
            }
            ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "msg": "", "data": [
                { "hpath": "/uwuwu/ADR/ADR-006: Доверие ok:false+retryable" },
                { "hpath": "/origa/ADR/ADR-006: Phrase Dataset Filtering" },
            ] }))
        })
        .mount(&server)
        .await;
    let sy = SiYuan::for_tests(server.uri());

    // Act
    let titles = sy.doc_titles(5000).await.unwrap();

    // Assert
    assert_eq!(
        titles,
        vec![
            "/uwuwu/ADR/ADR-006: Доверие ok:false+retryable".to_string(),
            "/origa/ADR/ADR-006: Phrase Dataset Filtering".to_string(),
        ]
    );
    let stmt = stmts.lock().unwrap().join("\n");
    assert!(
        stmt.contains("type='d'") && stmt.contains("limit 5000"),
        "фетч обязан нести явный LIMIT (кап ядра 64 строки): {stmt}"
    );
}

// ——— сборка stdout: порядок секций ———

/// SQL-роутер: fulltext-запрос (content like) → свой конверт,
/// доковый (type='d' limit) → свой (паттерн docs_tests).
async fn mount_sql_router(server: &MockServer, fulltext: Vec<Value>, docs: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body
                .get("stmt")
                .and_then(|s| s.as_str())
                .unwrap_or_default();
            let data = if stmt.contains("content like") {
                fulltext.clone()
            } else if stmt.contains("type='d' limit") {
                docs.clone()
            } else {
                Vec::new()
            };
            ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "msg": "", "data": data }))
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn search_lines_orders_fulltext_then_doc_section() {
    // Arrange: fulltext находит архивный блок, doc-секция — живые доки
    let server = MockServer::start().await;
    mount_sql_router(
        &server,
        vec![json!({
            "hpath": "/affine/wiki_ops find/children",
            "content": "ретраи по контракту ADR-006",
        })],
        vec![
            json!({ "hpath": "/uwuwu/ADR/ADR-006: Доверие ok:false+retryable" }),
            json!({ "hpath": "/origa/ADR/ADR-006: Phrase Dataset Filtering" }),
        ],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    // Act
    let lines = search_lines(&sy, "ADR-006", 10).await.unwrap();

    // Assert: без головы — её печатает search_run до фетча (отказный путь
    // stdout прежний), полнотекст раньше doc-секции
    assert_eq!(
        lines,
        vec![
            "1. /affine/wiki_ops find/children".to_string(),
            "    ретраи по контракту ADR-006".to_string(),
            "Доки (по hpath):".to_string(),
            "1. /uwuwu/ADR/ADR-006: Доверие ok:false+retryable".to_string(),
            "2. /origa/ADR/ADR-006: Phrase Dataset Filtering".to_string(),
        ]
    );
}

#[tokio::test]
async fn search_lines_empty_everywhere_reports_no_hits_and_omits_doc_section() {
    // Arrange
    let server = MockServer::start().await;
    mount_sql_router(&server, vec![], vec![]).await;
    let sy = SiYuan::for_tests(server.uri());

    // Act
    let lines = search_lines(&sy, "нет такого", 10).await.unwrap();

    // Assert: пустая doc-секция не печатается вовсе
    assert_eq!(lines, vec!["ничего не найдено".to_string()]);
}

#[tokio::test]
async fn search_lines_propagates_fulltext_fetch_failure() {
    // Arrange: ядро недоступно (порт закрыт) — мгновенный пейсинг ретраев
    let server = MockServer::start().await;
    let uri = server.uri();
    drop(server);
    let sy = SiYuan::for_tests(uri);

    // Act
    let result = search_lines(&sy, "ADR-006", 10).await;

    // Assert: отказ полнотекста — Err (голова уже напечатана search_run),
    // doc-секция со своей деградацией не маскирует сбой полнотекста
    assert!(result.is_err(), "ожидался Err на недоступном ядре");
}
