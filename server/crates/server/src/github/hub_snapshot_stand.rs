//! Стенд тестов снимка доски: двойник GraphQL-документов и ответы в форме
//! живого GitHub. HTTP-моков нет (ADR-110) — документы подменяются
//! замыканием на границе `read_board_pages`.

use super::*;

pub(super) const BOARD: &str = "Задачи uwuwu";
pub(super) const BOARD_ID: &str = "PVT_uwuwu";

/// Двойник GraphQL-документов: очередь ответов по порядку вызовов и
/// журнал (документ, переменные). Пустая очередь — лишний вызов, громкий
/// отказ.
#[derive(Default)]
pub(super) struct FakeGql {
    responses: std::sync::Mutex<std::collections::VecDeque<Value>>,
    calls: std::sync::Mutex<Vec<(&'static str, Value)>>,
}

impl FakeGql {
    fn push(&self, response: Value) {
        self.responses
            .lock()
            .expect("responses lock")
            .push_back(response);
    }

    pub(super) fn fetch(
        &self,
        query: &'static str,
        variables: Value,
    ) -> std::future::Ready<UsecaseResult<Value>> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((query, variables));
        let answer = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| UsecaseError::Board("fake gql: вызов без ответа".into()));
        std::future::ready(answer)
    }

    /// Вызов номер `n` (с нуля): документ и его переменные.
    pub(super) fn call(&self, n: usize) -> (&'static str, Value) {
        self.calls
            .lock()
            .expect("calls lock")
            .get(n)
            .cloned()
            .expect("вызов был")
    }
}

/// Ответ PROJECTS_QUERY: список досок хаба.
fn boards_list(boards: &[(&str, &str)]) -> Value {
    let nodes: Vec<Value> = boards
        .iter()
        .map(|(id, title)| json!({ "id": id, "title": title }))
        .collect();
    json!({ "repository": { "projectsV2": { "nodes": nodes } } })
}

/// Ответ BOARD_ITEMS_QUERY: `count` issue начиная с номера `first`;
/// `next` — курсор следующей страницы (None — страница последняя).
pub(super) fn items_page(first: u64, count: u64, next: Option<&str>) -> Value {
    let nodes: Vec<Value> = (first..first + count)
        .map(|n| {
            json!({
                "id": format!("IT_{n}"),
                "content": { "__typename": "Issue", "number": n,
                             "title": format!("задача {n}"), "state": "open" },
                "fieldValues": { "nodes": [
                    { "__typename": "ProjectV2ItemFieldSingleSelectValue",
                      "name": "Todo", "field": { "name": "Status" } }
                ] }
            })
        })
        .collect();
    json!({
        "node": {
            "title": BOARD,
            "field": { "id": "PVTSSF_status",
                       "options": [{ "id": "o1", "name": "Todo" },
                                   { "id": "o2", "name": "In Progress" }] },
            "items": { "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                       "nodes": nodes }
        }
    })
}

/// Стенд: список досок, затем страницы строк доски `BOARD` по порядку.
pub(super) fn staged(boards: &[(&str, &str)], pages: &[Value]) -> FakeGql {
    let gql = FakeGql::default();
    gql.push(boards_list(boards));
    for page in pages {
        gql.push(page.clone());
    }
    gql
}
