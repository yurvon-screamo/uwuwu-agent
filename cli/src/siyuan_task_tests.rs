//! Тесты read_rows: пагинация MCP database render против мока SiYuan.
//!
//! Мок слушает 127.0.0.1:0 (фиксированные порты — гонки, урок афин-эпохи).
//! Один запрос на соединение: каждый ответ несёт Connection: close, после
//! записи — shutdown сокета; reqwest не переиспользует соединение, а mcp_call
//! ретраев не имеет → детерминированный мок без reset-флаков на пуле.

use crate::siyuan::SiYuan;
use crate::siyuan_task::{read_rows, SyBoard, SyTask};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Спека мок-доски: сколько строк всего, размер страницы, есть ли rowCount
/// в ответе, инъекция дубля item_id последней строки предыдущей страницы
/// в начало текущей (дрейф страниц при конкурентных правках доски).
#[derive(Clone)]
struct MockBoard {
    total_rows: u64,
    page_size: u64,
    show_row_count: bool,
    dup_last_of_prev: bool,
}

fn mock_board(total_rows: u64) -> MockBoard {
    MockBoard {
        total_rows,
        page_size: 50,
        show_row_count: true,
        dup_last_of_prev: false,
    }
}

struct MockServer {
    base: String,
    pages: Arc<Mutex<Vec<u64>>>,
}

/// Запустить мок MCP-сервера: base-URL + лог запрошенных страниц render.
async fn spawn_mock(board: MockBoard) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("mock addr");
    let pages = Arc::new(Mutex::new(Vec::new()));
    let pages_for_tasks = pages.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let board = board.clone();
            let pages = pages_for_tasks.clone();
            tokio::spawn(async move { serve_one(stream, board, pages).await });
        }
    });
    MockServer {
        base: format!("http://{addr}"),
        pages,
    }
}

/// Один запрос на соединение: ответ с Connection: close, затем shutdown.
async fn serve_one(mut stream: TcpStream, board: MockBoard, pages: Arc<Mutex<Vec<u64>>>) {
    let Some(body) = read_request_body(&mut stream).await else {
        return;
    };
    let Ok(req) = serde_json::from_str::<Value>(&body) else {
        return;
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let (status, extra_headers, payload) = if method.starts_with("notifications/") {
        ("202 Accepted".to_string(), String::new(), String::new())
    } else if method == "initialize" {
        let result = json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "serverInfo": { "name": "mock-siyuan", "version": "0" }
        });
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        (
            "200 OK".to_string(),
            "mcp-session-id: mock-session\r\n".to_string(),
            reply.to_string(),
        )
    } else if method == "tools/call" {
        let args = req
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(json!({}));
        let reply = match args.get("action").and_then(|a| a.as_str()) {
            Some("render") => {
                let page = args.get("page").and_then(|p| p.as_u64()).unwrap_or(1);
                pages.lock().expect("pages lock").push(page);
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "isError": false, "structuredContent": {
                        "action": "render",
                        "data": { "view": render_view(&board, page) },
                    }}
                })
            }
            other => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("mock: action {other:?} не поддержан") }
            }),
        };
        ("200 OK".to_string(), String::new(), reply.to_string())
    } else {
        let reply = json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": format!("mock: метод {method} не поддержан") }
        });
        ("200 OK".to_string(), String::new(), reply.to_string())
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(payload.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Прочитать один HTTP-запрос: до конца заголовков, затем Content-Length байт тела.
async fn read_request_body(stream: &mut TcpStream) -> Option<String> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&raw[..header_end]).to_lowercase();
    let content_length = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while raw.len() < header_end + content_length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(raw[header_end..].to_vec()).ok()
}

/// Страница доски в форме живого ответа: view.{columns, rows, rowCount}.
fn render_view(board: &MockBoard, page: u64) -> Value {
    let mut rows = Vec::new();
    if board.dup_last_of_prev && page > 1 {
        let prev_last = (page - 1) * board.page_size - 1;
        rows.push(mock_row(prev_last));
    }
    let start = (page - 1) * board.page_size;
    let end = (start + board.page_size).min(board.total_rows);
    let mut idx = start;
    while idx < end {
        rows.push(mock_row(idx));
        idx += 1;
    }
    let mut view = json!({
        "columns": [
            { "id": "key-title", "name": "Title" },
            { "id": "key-status", "name": "Status" },
        ],
        "rows": rows,
    });
    if board.show_row_count {
        view["rowCount"] = json!(board.total_rows);
    }
    view
}

fn mock_row(idx: u64) -> Value {
    json!({
        "id": format!("row-{idx:04}"),
        "cells": [
            { "value": { "keyID": "key-title", "text": { "content": format!("Задача {idx:04}") } } },
            { "value": { "keyID": "key-status", "mSelect": [{ "content": "Draft" }] } },
        ],
    })
}

/// Мок-доска + клиент → (результат read_rows, лог страниц render).
async fn run_read_rows(board: MockBoard) -> (anyhow::Result<Vec<SyTask>>, Arc<Mutex<Vec<u64>>>) {
    let server = spawn_mock(board).await;
    let sy = SiYuan::for_tests(server.base.clone());
    let task_board = SyBoard {
        doc_id: "mock-doc".into(),
        av_id: "mock-av".into(),
        block_id: "mock-block".into(),
        hpath: "/Projects/mock/Задачи mock".into(),
        notebook: "Projects".into(),
    };
    let rows = read_rows(&sy, &task_board).await;
    (rows, server.pages.clone())
}

#[tokio::test]
async fn pages_all_rows_collected() {
    let (rows, pages) = run_read_rows(mock_board(105)).await;
    let rows = rows.expect("read_rows должен собрать все страницы доски");
    assert_eq!(
        rows.len(),
        105,
        "видны все строки, а не только первая страница"
    );
    assert_eq!(
        *pages.lock().expect("pages lock"),
        vec![1, 2, 3],
        "запрошены страницы 1–3; страница 4 не запрашивалась"
    );
}

#[tokio::test]
async fn terminates_on_empty_page_without_rowcount() {
    let (rows, pages) = run_read_rows(MockBoard {
        show_row_count: false,
        ..mock_board(57)
    })
    .await;
    let rows = rows.expect("терминация по пустой странице, без зависания");
    assert_eq!(rows.len(), 57);
    assert_eq!(
        *pages.lock().expect("pages lock"),
        vec![1, 2, 3],
        "третья страница пустая — терминация по пустой странице"
    );
}

#[tokio::test]
async fn single_page_no_extra_call() {
    let (rows, pages) = run_read_rows(mock_board(10)).await;
    let rows = rows.expect("read_rows на однопагинальной доске");
    assert_eq!(rows.len(), 10);
    assert_eq!(
        *pages.lock().expect("pages lock"),
        vec![1],
        "ровно один вызов render"
    );
}

#[tokio::test]
async fn cap_is_loud_error() {
    let (rows, pages) = run_read_rows(MockBoard {
        show_row_count: false,
        ..mock_board(6000)
    })
    .await;
    let err = rows.expect_err("кап страниц — громкая ошибка, не тихая усечка");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("усечена"),
        "диагностика усечки в ошибке: {msg}"
    );
    assert_eq!(
        pages.lock().expect("pages lock").len(),
        100,
        "запрошено ровно 100 страниц"
    );
}

#[tokio::test]
async fn dedup_page_drift() {
    let (rows, pages) = run_read_rows(MockBoard {
        dup_last_of_prev: true,
        ..mock_board(105)
    })
    .await;
    let rows = rows.expect("read_rows с инъекцией дубля на стыке страниц");
    assert_eq!(rows.len(), 105, "дубль на стыке страниц отфильтрован");
    assert_eq!(*pages.lock().expect("pages lock"), vec![1, 2, 3]);
    let mut ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 105, "все item_id уникальны");
}

/// Секция «## Комменты» лениво создаётся перед первым комментом: тело без
/// секции получает заголовок (1 append), повторный вызов не дублирует его.
/// Гэп ADR-014: комменты писались голыми буллетами без секции — thread
/// отвечал «комментов нет» при живых комментах.
#[tokio::test]
async fn comment_creates_comments_section_once() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DOC_ID: &str = "20261104120000-abcdefg";
    let server = MockServer::start().await;
    let export_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let calls = export_calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with(move |_req: &wiremock::Request| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let content = if n == 0 {
                "# Таск\n\n## Постановка\nтело\n".to_string()
            } else {
                "# Таск\n\n## Постановка\nтело\n\n## Комменты\n- **AGENTS**: строка\n".to_string()
            };
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "msg": "", "data": { "content": content } }))
        })
        .mount(&server)
        .await;

    let appends_log = appends.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            appends_log.lock().expect("appends lock").push(
                body.get("data")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "msg": "", "data": null }))
        })
        .mount(&server)
        .await;

    let sy = SiYuan::for_tests(server.uri());
    crate::siyuan_task::ensure_comments_section(&sy, DOC_ID)
        .await
        .expect("первый вызов: секция создана");
    crate::siyuan_task::ensure_comments_section(&sy, DOC_ID)
        .await
        .expect("повторный вызов: без дубля заголовка");

    let appends = appends.lock().expect("appends lock");
    assert_eq!(
        appends.len(),
        1,
        "заголовок секции append'ится ровно один раз"
    );
    assert_eq!(appends[0], "## Комменты");
}

/// Поведение appendBlock мока: успех; «применено, ответ потерян» (запись в
/// контенте есть, клиент видит code≠0); отказ без применения.
#[derive(Clone, Copy)]
enum AppendBehavior {
    Ok,
    AppliedButLost,
    Rejected,
}

/// Состояние мок-дока: экспорт отдаёт текущий контент, appendBlock мутирует
/// его и/или «роняет» ответ согласно behavior.
struct DocMock {
    server: wiremock::MockServer,
    content: Arc<Mutex<String>>,
    appends: Arc<Mutex<Vec<String>>>,
}

async fn spawn_doc_mock(initial: &str, behavior: AppendBehavior) -> DocMock {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const BODY_DOC: &str = "20261105120000-bounddo1";
    let server = MockServer::start().await;
    let content = Arc::new(Mutex::new(initial.to_string()));
    let appends = Arc::new(Mutex::new(Vec::new()));

    // резолв тела: SQL по id (живость привязки) + getIDsByHPath по тайтлу
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
            let data = if stmt.contains(&format!("id='{BODY_DOC}'")) {
                json!([{ "id": BODY_DOC }])
            } else {
                json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(json!({ "code": 0, "data": data }).to_string())
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = if hpath.ends_with("таск") {
                json!(["20261105120000-titleb2"])
            } else {
                json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(json!({ "code": 0, "data": data }).to_string())
        })
        .mount(&server)
        .await;

    let content_for_export = content.clone();
    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with(move |_req: &wiremock::Request| {
            let md = content_for_export.lock().expect("content lock").clone();
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "msg": "", "data": { "content": md } }))
        })
        .mount(&server)
        .await;

    let content_for_append = content.clone();
    let appends_log = appends.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let data = body
                .get("data")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            if matches!(
                behavior,
                AppendBehavior::Ok | AppendBehavior::AppliedButLost
            ) {
                content_for_append
                    .lock()
                    .expect("content lock")
                    .push_str(&data);
            }
            appends_log.lock().expect("appends lock").push(data);
            // «применено, ответ потерян»: запись в контенте есть, клиент видит сбой
            let code = match behavior {
                AppendBehavior::Ok => 0,
                AppendBehavior::AppliedButLost | AppendBehavior::Rejected => -1,
            };
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": code, "msg": "lost", "data": null }))
        })
        .mount(&server)
        .await;

    DocMock {
        server,
        content,
        appends,
    }
}

const TS: &str = "2026-11-05 10:00";

#[tokio::test]
async fn comment_applied_but_lost_writes_recovered_without_duplicates() {
    let mock = spawn_doc_mock(
        "# Таск\n\n## Постановка\nтело\n",
        AppendBehavior::AppliedButLost,
    )
    .await;
    let sy = SiYuan::for_tests(mock.server.uri());
    let board = test_board();
    let task = test_task();

    crate::siyuan_task::comment_with_ts(&sy, &board, &task, &draft(None), TS)
        .await
        .expect("записи применены, ответы потеряны — контрольные чтения узнают свои");

    let content = mock.content.lock().expect("content lock");
    assert_eq!(
        content.matches("## Комменты").count(),
        1,
        "заголовок секции без дублей: {content}"
    );
    assert_eq!(
        content
            .matches(&format!("- **AGENTS** ({TS}): привет"))
            .count(),
        1,
        "коммент без дублей (слепой ретрай дал бы копии): {content}"
    );
    let appends = mock.appends.lock().expect("appends lock");
    assert_eq!(
        appends.len(),
        2,
        "ровно заголовок + коммент, без слепых ретраев"
    );
}

#[tokio::test]
async fn comment_precheck_skips_line_already_in_doc() {
    let seeded = "# Таск\n\n## Комменты\n- **AGENTS** (2026-11-05 10:00): привет\n";
    let mock = spawn_doc_mock(seeded, AppendBehavior::Ok).await;
    let sy = SiYuan::for_tests(mock.server.uri());
    let board = test_board();
    let task = test_task();

    crate::siyuan_task::comment_with_ts(&sy, &board, &task, &draft(None), TS)
        .await
        .expect("идемпотентный повтор: коммент уже в доке");

    let appends = mock.appends.lock().expect("appends lock");
    assert!(
        appends.is_empty(),
        "повторный коммент не постится — гвард по свежему чтению: {appends:?}"
    );
}

#[tokio::test]
async fn comment_precheck_matches_indented_reply_lines() {
    let seeded = "# Таск\n\n## Комменты\n  - **AGENTS** (2026-11-05 10:00): привет\n";
    let mock = spawn_doc_mock(seeded, AppendBehavior::Ok).await;
    let sy = SiYuan::for_tests(mock.server.uri());
    let board = test_board();
    let task = test_task();

    crate::siyuan_task::comment_with_ts(&sy, &board, &task, &draft(Some("20261105-thread")), TS)
        .await
        .expect("реплай-дубль узнаётся по контенту строки без отступа");

    let appends = mock.appends.lock().expect("appends lock");
    assert!(
        appends.is_empty(),
        "отступ не ломает матчинг дубля: {appends:?}"
    );
}

#[tokio::test]
async fn comment_honest_failure_when_write_not_applied() {
    let mock = spawn_doc_mock("# Таск\n\n## Комменты\n", AppendBehavior::Rejected).await;
    let sy = SiYuan::for_tests(mock.server.uri());
    let board = test_board();
    let task = test_task();

    let err = crate::siyuan_task::comment_with_ts(&sy, &board, &task, &draft(None), TS)
        .await
        .expect_err("запись не применена — честный отказ без выдуманного успеха");
    assert!(
        err.to_string().contains("appendBlock"),
        "суть сбоя названа: {err}"
    );
    assert_eq!(
        mock.appends.lock().expect("appends lock").len(),
        1,
        "один POST, без слепых ретраев"
    );
}

fn draft<'a>(reply_to: Option<&'a str>) -> crate::siyuan_task::CommentDraft<'a> {
    crate::siyuan_task::CommentDraft {
        author: "AGENTS",
        text: "привет",
        reply_to,
    }
}

fn test_board() -> SyBoard {
    SyBoard {
        doc_id: "board-doc".into(),
        av_id: "av-1".into(),
        block_id: "block-1".into(),
        hpath: "/uwuwu/Задачи uwuwu".into(),
        notebook: "20261103224312-fbrf5fp".into(),
    }
}

fn test_task() -> SyTask {
    SyTask {
        item_id: "item-1".into(),
        title: "таск".into(),
        status: "Draft".into(),
        branch: String::new(),
        pr: String::new(),
        schedule: String::new(),
        last_run: String::new(),
        dev_session: String::new(),
        body_doc: Some("20261105120000-bounddo1".into()),
    }
}
