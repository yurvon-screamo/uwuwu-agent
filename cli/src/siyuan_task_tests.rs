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
    MockBoard { total_rows, page_size: 50, show_row_count: true, dup_last_of_prev: false }
}

struct MockServer {
    base: String,
    pages: Arc<Mutex<Vec<u64>>>,
}

/// Запустить мок MCP-сервера: base-URL + лог запрошенных страниц render.
async fn spawn_mock(board: MockBoard) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock listener");
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
    MockServer { base: format!("http://{addr}"), pages }
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
        let args = req.pointer("/params/arguments").cloned().unwrap_or(json!({}));
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
    assert_eq!(rows.len(), 105, "видны все строки, а не только первая страница");
    assert_eq!(
        *pages.lock().expect("pages lock"),
        vec![1, 2, 3],
        "запрошены страницы 1–3; страница 4 не запрашивалась"
    );
}

#[tokio::test]
async fn terminates_on_empty_page_without_rowcount() {
    let (rows, pages) = run_read_rows(MockBoard { show_row_count: false, ..mock_board(57) }).await;
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
    assert_eq!(*pages.lock().expect("pages lock"), vec![1], "ровно один вызов render");
}

#[tokio::test]
async fn cap_is_loud_error() {
    let (rows, pages) =
        run_read_rows(MockBoard { show_row_count: false, ..mock_board(6000) }).await;
    let err = rows.expect_err("кап страниц — громкая ошибка, не тихая усечка");
    let msg = format!("{err:#}");
    assert!(msg.contains("усечена"), "диагностика усечки в ошибке: {msg}");
    assert_eq!(pages.lock().expect("pages lock").len(), 100, "запрошено ровно 100 страниц");
}

#[tokio::test]
async fn dedup_page_drift() {
    let (rows, pages) = run_read_rows(MockBoard { dup_last_of_prev: true, ..mock_board(105) }).await;
    let rows = rows.expect("read_rows с инъекцией дубля на стыке страниц");
    assert_eq!(rows.len(), 105, "дубль на стыке страниц отфильтрован");
    assert_eq!(*pages.lock().expect("pages lock"), vec![1, 2, 3]);
    let mut ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 105, "все item_id уникальны");
}
