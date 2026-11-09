//! Тесты `docs ls`: clap-грамматика, SQL-контракт выборки поддерева
//! (substr в символах, экранирование, явный LIMIT), вычисление прямых
//! детей и поведение listing на wiremock-ядре (пусто / раскладка
//! доступных / обрезка лимита).

use super::cli::{Cli, Commands, DocsSub};
use super::docs_ls::{direct_children, ls_listing, subtree_stmt, top_segments_stmt};
use crate::siyuan::SiYuan;
use clap::Parser as _;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn parse_docs(args: &[&str]) -> clap::error::Result<Commands> {
    let mut argv = vec!["uwuwu-cli", "docs"];
    argv.extend_from_slice(args);
    Ok(Cli::try_parse_from(argv)?.command)
}

#[test]
fn ls_parses_prefix_slug() {
    let Commands::Docs { command } = parse_docs(&["ls", "uwuwu/ADR"]).unwrap() else {
        panic!("expected docs subcommand");
    };
    // DocsSub с одной вариацией — паттерн irrefutable
    let DocsSub::Ls { prefix } = command;
    assert_eq!(prefix, "uwuwu/ADR");
}

#[test]
fn ls_requires_prefix() {
    assert!(parse_docs(&["ls"]).is_err());
}

#[test]
fn subtree_stmt_counts_chars_not_bytes() {
    // substr в SQLite считает символы, не байты: кириллический слаг
    // «/uwuwu/Задачи/» — 14 символов, 28 байт; байтовая длина съела бы хвост
    let stmt = subtree_stmt("uwuwu/Задачи", 10000);
    assert!(stmt.contains("substr(hpath,1,14)"), "{stmt}");
}

#[test]
fn subtree_stmt_escapes_quotes_and_carries_limit_and_order() {
    let stmt = subtree_stmt("it's/ADR", 501);
    assert!(stmt.contains("'/it''s/ADR/'"), "{stmt}");
    assert!(stmt.contains("limit 501"), "{stmt}");
    assert!(stmt.contains("type='d'"), "{stmt}");
    assert!(stmt.contains("order by hpath"), "{stmt}");
}

#[test]
fn children_split_docs_and_dirs_with_dedup() {
    let hpaths = [
        "/uwuwu/ADR/ADR-001: x",
        "/uwuwu/ADR/ADR-002: y",
        "/uwuwu/Ранбуки/деплой",
        "/uwuwu/Ранбуки/ревью",
        "/uwuwu/README (uwuwu)",
    ];
    let children = direct_children("uwuwu", hpaths.into_iter());
    assert_eq!(children, vec!["ADR/", "README (uwuwu)", "Ранбуки/"]);
}

#[test]
fn children_pass_through_titles_with_apostrophes() {
    let hpaths = ["/uwuwu/ADR/ADR-050: агент's guide"];
    let children = direct_children("uwuwu/ADR", hpaths.into_iter());
    assert_eq!(children, vec!["ADR-050: агент's guide"]);
}

#[test]
fn children_merge_folder_doc_with_its_dir_entry() {
    // папка-док «ADR» и дети под ним — одна запись «ADR/», не «ADR» + «ADR/»
    let hpaths = ["/uwuwu/ADR", "/uwuwu/ADR/ADR-001: x", "/uwuwu/README"];
    let children = direct_children("uwuwu", hpaths.into_iter());
    assert_eq!(children, vec!["ADR/", "README"]);
}

#[test]
fn top_segments_depth_matches_subtree_semantics() {
    // раскладка признаёт пространство с прямыми доками (глубина ≥2 сегментов)
    // — иначе плоские пространства ловят ложный отказ «не найдено»
    // (ревью PR #124, Common); одиночные hpath Daily/Experience отсечены
    let stmt = top_segments_stmt();
    assert!(stmt.contains("hpath like '/%/%'"), "{stmt}");
    assert!(!stmt.contains("hpath like '/%/%/%'"), "{stmt}");
}

/// Роутер SQL-конвертов: top_segments-запрос (distinct substr) отвечает
/// раскладкой пространств, поддерево — своими строками.
fn sql_router(
    subtree: Vec<Value>,
    tops: Vec<Value>,
) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
    move |req: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let stmt = body
            .get("stmt")
            .and_then(|s| s.as_str())
            .unwrap_or_default();
        let data = if stmt.contains("distinct substr") {
            &tops
        } else {
            &subtree
        };
        ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "msg": "", "data": data }))
    }
}

async fn mount_sql_router(server: &MockServer, subtree: Vec<Value>, tops: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(sql_router(subtree, tops))
        .mount(server)
        .await;
}

#[tokio::test]
async fn listing_returns_direct_children_with_dirs_marked() {
    let server = MockServer::start().await;
    mount_sql_router(
        &server,
        vec![
            json!({ "hpath": "/uwuwu/ADR/ADR-001: x" }),
            json!({ "hpath": "/uwuwu/ADR/archive/old" }),
        ],
        vec![],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let listing = ls_listing(&sy, "uwuwu/ADR", 10000).await.unwrap();

    assert_eq!(
        listing.children,
        Some(vec!["ADR-001: x".to_string(), "archive/".to_string()])
    );
    assert!(!listing.truncated);
}

#[tokio::test]
async fn known_space_without_children_is_empty() {
    let server = MockServer::start().await;
    mount_sql_router(&server, vec![], vec![json!({ "top": "uwuwu" })]).await;
    let sy = SiYuan::for_tests(server.uri());

    let listing = ls_listing(&sy, "uwuwu/нет-такой-папки", 10000)
        .await
        .unwrap();

    assert_eq!(listing.children, None);
    assert!(!listing.truncated);
}

#[tokio::test]
async fn unknown_space_bails_with_available_spaces() {
    let server = MockServer::start().await;
    mount_sql_router(
        &server,
        vec![],
        vec![json!({ "top": "platform" }), json!({ "top": "uwuwu" })],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let err = ls_listing(&sy, "опечатка/ADR", 10000)
        .await
        .expect_err("unknown space must bail");

    let text = err.to_string();
    assert!(text.contains("«опечатка» не найдено"), "{text}");
    assert!(text.contains("доступные: platform, uwuwu"), "{text}");
}

#[tokio::test]
async fn subtree_beyond_limit_is_marked_truncated() {
    // subtree_limit=2, ядро отдало fetch_limit+1=3 строку — обрезка по факту
    let server = MockServer::start().await;
    mount_sql_router(
        &server,
        vec![
            json!({ "hpath": "/uwuwu/ADR/ADR-001: x" }),
            json!({ "hpath": "/uwuwu/ADR/ADR-002: y" }),
            json!({ "hpath": "/uwuwu/ADR/ADR-003: z" }),
        ],
        vec![],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let listing = ls_listing(&sy, "uwuwu/ADR", 2).await.unwrap();

    assert!(listing.truncated);
    assert_eq!(
        listing.children,
        Some(vec!["ADR-001: x".to_string(), "ADR-002: y".to_string()])
    );
}

#[tokio::test]
async fn subtree_exactly_at_limit_is_not_truncated() {
    // ровно лимит строк при fetch limit+1 — обрезки нет, ложный warning
    // пограничником исключён
    let server = MockServer::start().await;
    mount_sql_router(
        &server,
        vec![
            json!({ "hpath": "/uwuwu/ADR/ADR-001: x" }),
            json!({ "hpath": "/uwuwu/ADR/ADR-002: y" }),
        ],
        vec![],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let listing = ls_listing(&sy, "uwuwu/ADR", 2).await.unwrap();

    assert!(!listing.truncated);
    assert_eq!(
        listing.children,
        Some(vec!["ADR-001: x".to_string(), "ADR-002: y".to_string()])
    );
}

#[tokio::test]
async fn empty_prefix_bails_with_available_spaces() {
    let server = MockServer::start().await;
    mount_sql_router(&server, vec![], vec![json!({ "top": "uwuwu" })]).await;
    let sy = SiYuan::for_tests(server.uri());

    let err = ls_listing(&sy, "/", 10000)
        .await
        .expect_err("empty prefix must bail");

    let text = err.to_string();
    assert!(text.contains("пустой префикс"), "{text}");
    assert!(text.contains("доступные пространства: uwuwu"), "{text}");
}

/// Роутер ноутбучной ветки: lsNotebooks + box-sql (дети корня ноутбука)
/// + общий sql-роутер прежних веток.
async fn mount_notebook_router(
    server: &MockServer,
    notebooks: Vec<Value>,
    box_rows: Vec<Value>,
    subtree: Vec<Value>,
    tops: Vec<Value>,
) {
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "msg": "", "data": { "notebooks": notebooks } })),
        )
        .mount(server)
        .await;
    let sql = sql_router(subtree, tops);
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body
                .get("stmt")
                .and_then(|s| s.as_str())
                .unwrap_or_default();
            if stmt.contains("box=") {
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "code": 0, "msg": "", "data": box_rows }))
            } else {
                sql(req)
            }
        })
        .mount(server)
        .await;
}

/// Префикс-имя открытого ноутбука → дети его корня: проекты в ноутбуке
/// Projects не живут в hpath с именем ноутбука, перечень даёт box-выборка
/// (fix 20261109132340: путь из help projects и методичек).
#[tokio::test]
async fn notebook_prefix_lists_root_children() {
    let server = MockServer::start().await;
    mount_notebook_router(
        &server,
        vec![json!({ "id": "20261103224312", "name": "Projects", "closed": false })],
        vec![
            json!({ "hpath": "/uwuwu" }),
            json!({ "hpath": "/uwuwu/README (uwuwu)" }),
            json!({ "hpath": "/cli/ADR/ADR-014: SiYuan" }),
            json!({ "hpath": "/daily-note" }),
        ],
        vec![], // hpath-пространство «Projects» не существует
        vec![],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let listing = ls_listing(&sy, "Projects", 10000).await.unwrap();

    assert_eq!(
        listing.children,
        Some(vec![
            "cli/".to_string(),
            "daily-note".to_string(),
            "uwuwu/".to_string()
        ])
    );
    assert!(!listing.truncated);
}

/// Однотокенный неизвестный префикс после пустого notebook-фоллбека —
/// прежний громкий отказ с раскладкой доступных пространств.
#[tokio::test]
async fn single_token_unknown_bails_after_notebook_fallback() {
    let server = MockServer::start().await;
    mount_notebook_router(
        &server,
        vec![json!({ "id": "nb1", "name": "Projects", "closed": false })],
        vec![],
        vec![],
        vec![json!({ "top": "uwuwu" })],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let err = ls_listing(&sy, "опечатка", 10000)
        .await
        .expect_err("unknown space must bail");

    let text = err.to_string();
    assert!(text.contains("«опечатка» не найдено"), "{text}");
    assert!(text.contains("доступные: uwuwu"), "{text}");
}

/// notebook_children напрямую: тот же box-контракт без ls_listing-обвязки.
#[tokio::test]
async fn notebook_children_direct_resolves_by_name() {
    let server = MockServer::start().await;
    mount_notebook_router(
        &server,
        vec![json!({ "id": "20261103224312", "name": "Projects", "closed": false })],
        vec![
            json!({ "hpath": "/uwuwu" }),
            json!({ "hpath": "/uwuwu/README" }),
        ],
        vec![],
        vec![],
    )
    .await;
    let sy = SiYuan::for_tests(server.uri());
    let nbs = sy.notebooks().await.unwrap();
    eprintln!("notebooks: {nbs:?}");
    let direct = super::docs_ls::notebook_children(&sy, "Projects", 10000)
        .await
        .unwrap();
    eprintln!("children: {direct:?}");
    assert_eq!(direct, Some(vec!["uwuwu/".to_string()]));
}
