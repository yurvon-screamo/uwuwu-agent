//! Тесты чтения снимка доски (инцидент 23.11 — один GraphQL-документ нёс
//! строки всех досок хаба и падал по 502 у шлюза GitHub): форма документов
//! и постраничная логика поверх стенда `snapshot_stand`.

use super::snapshot_stand::{items_page, staged, FakeGql, BOARD, BOARD_ID};
use super::*;

async fn read(gql: &FakeGql) -> UsecaseResult<(Vec<Task>, BoardMeta)> {
    read_board_pages("o/r", BOARD, |query, variables| gql.fetch(query, variables)).await
}

/// Ожидаемый отказ чтения; паника, если снимок прочитался.
async fn read_err(gql: &FakeGql) -> UsecaseError {
    match read(gql).await {
        Ok(_) => panic!("снимок прочитался — ожидался отказ"),
        Err(e) => e,
    }
}

#[test]
fn snapshot_documents_never_join_items_into_project_search() {
    // GitHub матчит проекты нестрого: по слову «Задачи» в ответ приходят
    // все открытые доски — строки, присоединённые к поиску, умножаются
    // на их число, и документ перестаёт укладываться в лимит шлюза.
    assert!(
        !PROJECTS_QUERY.contains("items("),
        "список досок несёт строки"
    );
    assert!(
        !BOARD_ITEMS_QUERY.contains("projectsV2"),
        "строки читаются внутри поиска проектов"
    );
    assert!(
        BOARD_ITEMS_QUERY.contains("node(id:"),
        "строки адресованы не по id проекта"
    );
}

#[tokio::test]
async fn board_resolves_by_exact_title_not_by_search_match() {
    // Arrange: чужая доска с тем же словом «Задачи» стоит первой
    let gql = staged(
        &[("PVT_sandbox", "Задачи sandbox"), (BOARD_ID, BOARD)],
        &[items_page(1, 2, None)],
    );

    // Act
    let (_, meta) = read(&gql).await.expect("снимок прочитан");

    // Assert
    assert_eq!(gql.call(1).1["id"].as_str(), Some(BOARD_ID));
    assert_eq!(meta.project_id, BOARD_ID);
}

#[tokio::test]
async fn unknown_board_is_a_loud_error() {
    // Arrange
    let gql = staged(&[("PVT_sandbox", "Задачи sandbox")], &[]);

    // Act
    let err = read_err(&gql).await;

    // Assert
    assert!(err.to_string().contains("not found"), "{err}");
}

#[tokio::test]
async fn pages_follow_the_cursor_and_stop_after_the_last_page() {
    // Arrange
    let gql = staged(
        &[(BOARD_ID, BOARD)],
        &[
            items_page(1, 50, Some("c1")),
            items_page(51, 50, Some("c2")),
            items_page(101, 6, None),
        ],
    );

    // Act
    let (rows, _) = read(&gql).await.expect("снимок прочитан");

    // Assert: лишний вызов за последней страницей — отказ стенда
    let afters = (1..=3)
        .map(|n| gql.call(n).1["after"].clone())
        .collect::<Vec<_>>();
    assert_eq!(afters, vec![Value::Null, json!("c1"), json!("c2")]);
    assert_eq!(rows.len(), 106);
}

#[tokio::test]
async fn status_field_and_its_options_come_with_the_board() {
    // Arrange
    let gql = staged(&[(BOARD_ID, BOARD)], &[items_page(1, 1, None)]);

    // Act
    let (_, meta) = read(&gql).await.expect("снимок прочитан");

    // Assert
    assert_eq!(meta.status_field_id, "PVTSSF_status");
    assert_eq!(
        meta.options.get("In Progress").map(String::as_str),
        Some("o2")
    );
}

#[tokio::test]
async fn items_page_carries_the_bounded_page_size() {
    // Arrange
    let gql = staged(&[(BOARD_ID, BOARD)], &[items_page(1, 1, None)]);

    // Act
    read(&gql).await.expect("снимок прочитан");

    // Assert: страница держится в секундах — запас к лимиту шлюза
    assert_eq!(
        gql.call(1).1["first"].as_u64(),
        Some(50),
        "размер страницы снимка изменился — проверь запас к лимиту шлюза"
    );
}

#[tokio::test]
async fn endless_pagination_stops_at_the_page_guard() {
    // Arrange: каждая страница обещает следующую
    let pages = vec![items_page(1, 1, Some("same")); MAX_PAGES];
    let gql = staged(&[(BOARD_ID, BOARD)], &pages);

    // Act
    let err = read_err(&gql).await;

    // Assert
    assert!(err.to_string().contains("exceeded"), "{err}");
}

#[tokio::test]
async fn unreadable_project_node_is_a_loud_error() {
    // Arrange: id резолвился, а узел проектом не отвечает
    let gql = staged(&[(BOARD_ID, BOARD)], &[json!({ "node": null })]);

    // Act
    let err = read_err(&gql).await;

    // Assert
    assert!(err.to_string().contains("not readable"), "{err}");
}

/// Живой e2e снимка (образец — goals_live_tests): реальный хаб, гвард
/// UWUWU_E2E=1. Держит контракт документов против живого GraphQL — то,
/// чего стенд не видит: форма `node(id)`, `field(name:"Status")`, курсор.
#[tokio::test]
async fn live_snapshot_reads_configured_boards() {
    if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
        return;
    }
    let token = std::env::var("GH_TOKEN").unwrap_or_default();
    assert!(!token.is_empty(), "UWUWU_E2E=1: нужен GH_TOKEN");
    let hub = GitHubHub::new("yurvon-screamo/uwuwu-hub".into(), token);
    for board in [
        "Задачи uwuwu",
        "Задачи origa",
        "Задачи true-washing",
        "Задачи foilcap",
        "Задачи 1xgames",
    ] {
        let started = std::time::Instant::now();
        let rows = hub
            .read_board(board)
            .await
            .unwrap_or_else(|e| panic!("живой снимок «{board}»: {e}"));
        // факт живого прогона: объём и время снимка каждой доски
        eprintln!(
            "«{board}»: {} строк за {} мс",
            rows.len(),
            started.elapsed().as_millis()
        );
        assert!(
            !rows.is_empty(),
            "живая доска «{board}» пуста — носитель или запрос"
        );
    }
}
