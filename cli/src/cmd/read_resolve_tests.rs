//! Тесты резолва `read` (read_resolve): точные формы, like-кандидаты на
//! промахе (контекст проектной части слага → глобальный фолбэк),
//! авторезолв уникального кандидата, guard'ы и SQL-контракт (экранирование,
//! порядок, лимит+1). Ядро мокается wiremock'ом — роутер матчит stmt по
//! подстрокам, паттерн docs_tests; журнал stmt ловит контракт запросов.

use super::read_resolve::resolve;
use crate::siyuan::SiYuan;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Журнал stmt: контрактные проверки (экранирование, order by, guard'ы).
type StmtLog = Arc<Mutex<Vec<String>>>;

fn doc(id: &str, hpath: &str, box_id: &str) -> Value {
    json!({ "id": id, "hpath": hpath, "box": box_id })
}

/// Роутер SQL-конвертов: stmt → строки по первой подошедшей подстроке
/// (специфичные фрагменты держать раньше общих: like-паттерн содержит
/// глобальный фрагмент как подстроку).
fn log_router(
    routes: Vec<(&'static str, Vec<Value>)>,
    log: StmtLog,
) -> impl Fn(&Request) -> ResponseTemplate {
    move |req: &Request| {
        let stmt = serde_json::from_slice::<Value>(&req.body)
            .unwrap_or_default()
            .get("stmt")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        log.lock().expect("stmt log").push(stmt.clone());
        let data = routes
            .iter()
            .find(|(frag, _)| stmt.contains(frag))
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default();
        ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "msg": "", "data": data }))
    }
}

async fn mount_kernel(routes: Vec<(&'static str, Vec<Value>)>) -> (MockServer, StmtLog) {
    let server = MockServer::start().await;
    let log: StmtLog = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(log_router(routes, log.clone()))
        .mount(&server)
        .await;
    // справочник ноутбуков: подсказка/пометка не должны падать на пустом
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "msg": "", "notebooks": [] })),
        )
        .mount(&server)
        .await;
    (server, log)
}

const UWUWU_ADR030: &str = "/uwuwu/ADR/ADR-030: Тихий протокол тика крона";
const ORIGA_ADR030: &str = "/origa/ADR/ADR-030: Cyrillic typography";
const HOUSEKEEP: &str = "/platform/Фабрика uwuwu: Design хаускипа — факт-сверка постановки";

fn logged(log: &StmtLog) -> Vec<String> {
    log.lock().expect("stmt log").clone()
}

#[tokio::test]
async fn exact_hpath_slug_resolves_without_like_queries() {
    let (server, log) = mount_kernel(vec![(
        "hpath = '/uwuwu/README'",
        vec![doc("d-readme", "/uwuwu/README", "nb-uwuwu")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "uwuwu/README").await.unwrap();

    assert_eq!(id, "d-readme");
    // только точный hpath-запрос — like-ступень не выполняется
    assert!(
        logged(&log).iter().all(|stmt| !stmt.contains("like")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn exact_title_of_last_segment_resolves() {
    let (server, _log) = mount_kernel(vec![(
        "content = 'ADR-030: Тихий протокол тика крона'",
        vec![doc("d-exact", UWUWU_ADR030, "nb-uwuwu")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "uwuwu/ADR-030: Тихий протокол тика крона")
        .await
        .unwrap();

    assert_eq!(id, "d-exact");
}

#[tokio::test]
async fn exact_title_ambiguity_lists_candidates() {
    let (server, _log) = mount_kernel(vec![(
        "content = 'README'",
        vec![
            doc("d-t1", "/daily/README", "nb-daily"),
            doc("d-t2", "/origa/README", "nb-origa"),
        ],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "README").await.unwrap_err().to_string();

    assert!(err.contains("неоднозначен (2 кандидатов)"), "{err}");
    assert!(err.contains("/daily/README"), "{err}");
    assert!(err.contains("/origa/README"), "{err}");
}

#[tokio::test]
async fn scoped_prefix_matching_one_doc_auto_resolves() {
    // witness-1: слаг с проектом, тайтл неполный — авторезолв одним вызовом
    let (server, log) = mount_kernel(vec![(
        "/uwuwu/%ADR-030%",
        vec![doc("d-uwuwu-030", UWUWU_ADR030, "nb-uwuwu")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "uwuwu/ADR-030").await.unwrap();

    assert_eq!(id, "d-uwuwu-030");
    assert!(
        logged(&log)
            .iter()
            .any(|stmt| stmt.contains("like '/uwuwu/%ADR-030%'")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn bare_segment_matching_two_docs_lists_both() {
    // witness-1 без проектной части: оба ADR-030 в подсказке, не авторезолв
    let (server, _log) = mount_kernel(vec![(
        "%ADR-030%",
        vec![
            doc("d-uwuwu-030", UWUWU_ADR030, "nb-uwuwu"),
            doc("d-origa-030", ORIGA_ADR030, "nb-origa"),
        ],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "ADR-030").await.unwrap_err().to_string();

    assert!(err.contains("не найден — похожие доки:"), "{err}");
    assert!(err.contains(UWUWU_ADR030), "{err}");
    assert!(err.contains(ORIGA_ADR030), "{err}");
    assert!(!err.contains("неоднозначен"), "{err}");
}

#[tokio::test]
async fn segment_inside_title_auto_resolves() {
    // witness-2: одно-символьное расхождение в середине тайтла — префикс
    // не матчится, hpath-«содержит» находит, уникальный кандидат → авторезолв
    let (server, _log) = mount_kernel(vec![(
        "%хаускип%",
        vec![doc("d-housekeep", HOUSEKEEP, "nb-platform")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "хаускип").await.unwrap();

    assert_eq!(id, "d-housekeep");
}

#[tokio::test]
async fn full_title_with_tail_divergence_falls_to_word_match() {
    // поверенный тайтл целиком, расхождение в середине слова («хаускип» vs
    // «хаускипа»): ступени 1–2 промахиваются, word-AND ловит по словам
    let believed = "Фабрика uwuwu: Design хаускип — факт-сверка постановки с живым репо";
    let (server, log) = mount_kernel(vec![(
        "hpath like '%хаускип%'",
        vec![doc("d-housekeep", HOUSEKEEP, "nb-platform")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, believed).await.unwrap();

    assert_eq!(id, "d-housekeep");
    assert!(
        logged(&log)
            .iter()
            .any(|stmt| stmt.contains("hpath like '%хаускип%' escape '\\' and ")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn wrong_project_prefix_unique_fallback_auto_resolves() {
    // исмазанная проектная часть: scoped пуст, глобальный фолбэк уникален
    let (server, log) = mount_kernel(vec![(
        "%хаускип%",
        vec![doc("d-housekeep", HOUSEKEEP, "nb-platform")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "origa/хаускип").await.unwrap();

    assert_eq!(id, "d-housekeep");
    assert!(
        logged(&log)
            .iter()
            .any(|stmt| stmt.contains("like '/origa/%хаускип%'")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn wrong_project_prefix_multiple_fallback_lists_both() {
    let (server, _log) = mount_kernel(vec![
        ("/origa/%ADR-030%", vec![]),
        (
            "%ADR-030%",
            vec![
                doc("d-uwuwu-030", UWUWU_ADR030, "nb-uwuwu"),
                doc("d-origa-030", ORIGA_ADR030, "nb-origa"),
            ],
        ),
    ])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "origa/ADR-030").await.unwrap_err().to_string();

    assert!(err.contains("похожие доки:"), "{err}");
    assert!(err.contains(UWUWU_ADR030), "{err}");
    assert!(err.contains(ORIGA_ADR030), "{err}");
}

#[tokio::test]
async fn no_candidates_leaves_plain_not_found() {
    let (server, _log) = mount_kernel(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "полный-промах").await.unwrap_err().to_string();

    assert_eq!(err, "док «полный-промах» не найден");
}

#[tokio::test]
async fn trailing_slash_slug_skips_like_queries() {
    let (server, log) = mount_kernel(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "uwuwu/").await.unwrap_err().to_string();

    assert_eq!(err, "док «uwuwu/» не найден");
    assert!(
        logged(&log).iter().all(|stmt| !stmt.contains("like")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn apostrophe_in_slug_is_escaped_in_like_stmt() {
    let (server, log) = mount_kernel(vec![(
        "/it''s/%ADR-030%",
        vec![doc("d-apost", "/it's/ADR-030: x", "nb-its")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "it's/ADR-030").await.unwrap();

    assert_eq!(id, "d-apost");
    assert!(
        logged(&log)
            .iter()
            .any(|stmt| stmt.contains("like '/it''s/%ADR-030%'")),
        "{:?}",
        logged(&log)
    );
}

#[tokio::test]
async fn leading_slash_is_normalized_before_matching() {
    // witness-форма «/uwuwu/ADR/ADR-030»: без нормализации префикс был бы «//uwuwu/…»
    let (server, log) = mount_kernel(vec![(
        "/uwuwu/%ADR-030%",
        vec![doc("d-uwuwu-030", UWUWU_ADR030, "nb-uwuwu")],
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());

    let id = resolve(&sy, "/uwuwu/ADR-030").await.unwrap();

    assert_eq!(id, "d-uwuwu-030");
    let stmts = logged(&log);
    assert!(
        stmts
            .iter()
            .any(|stmt| stmt.contains("like '/uwuwu/%ADR-030%'")),
        "{stmts:?}"
    );
    assert!(
        stmts.iter().all(|stmt| !stmt.contains("'//uwuwu")),
        "{stmts:?}"
    );
}

#[tokio::test]
async fn like_stmt_carries_order_and_limit_plus_one() {
    let (server, log) = mount_kernel(vec![("%ADR-030%", vec![])]).await;
    let sy = SiYuan::for_tests(server.uri());

    let _ = resolve(&sy, "ADR-030").await;

    let like = logged(&log)
        .into_iter()
        .find(|stmt| stmt.contains("like"))
        .expect("like query executed");
    assert!(like.contains("order by hpath"), "{like}");
    assert!(like.contains("limit 6"), "{like}");
}

#[tokio::test]
async fn truncated_candidate_list_marks_omission() {
    let rows: Vec<Value> = (0..6)
        .map(|i| {
            doc(
                &format!("d-{i}"),
                &format!("/uwuwu/ADR/ADR-05{i}: t{i}"),
                "nb-uwuwu",
            )
        })
        .collect();
    let (server, _log) = mount_kernel(vec![("/uwuwu/%ADR-05%", rows)]).await;
    let sy = SiYuan::for_tests(server.uri());

    let err = resolve(&sy, "uwuwu/ADR-05").await.unwrap_err().to_string();

    assert!(
        err.contains("показаны первые 5, кандидатов больше"),
        "{err}"
    );
    for i in 0..5 {
        assert!(err.contains(&format!("ADR-05{i}")), "{err}");
    }
    assert!(!err.contains("ADR-055"), "{err}");
}
