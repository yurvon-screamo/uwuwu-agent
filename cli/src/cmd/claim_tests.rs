//! claim_tests.rs — `task claim`: запись сессионной ячейки «Сессия dev»
//! на независимом wiremock-ядре (только MCP: keys + item_update).
//!
//! Контракт: колонка обязательна (громкий отказ без записи), перезапись
//! чужой сессии легитимна (перехват работы), значение — text-контент.

use crate::siyuan::SiYuan;
use crate::siyuan_task::{self as syt, SyBoard, SyTask};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

struct Kernel {
    /// Есть ли колонка «Сессия dev» на доске.
    has_session_key: AtomicBool,
    /// (keyID, itemID, value) всех item_update.
    item_updates: Mutex<Vec<(String, String, serde_json::Value)>>,
}

impl Kernel {
    fn with_session_key(present: bool) -> Arc<Self> {
        Arc::new(Self {
            has_session_key: AtomicBool::new(present),
            item_updates: Mutex::new(Vec::new()),
        })
    }

    fn updates(&self) -> Vec<(String, String, serde_json::Value)> {
        self.item_updates.lock().unwrap().clone()
    }
}

fn mcp_text(payload: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
        })
        .to_string(),
    )
}

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-claim");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if !body.contains("tools/call") {
                    return ResponseTemplate::new(200).set_body_string(
                        r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                    );
                }
                if body.contains("\"database\"") && body.contains("\"keys\"") {
                    let mut keys = serde_json::json!([
                        { "id": "key-title", "name": "Primary Key" },
                        { "id": "key-status", "name": "Status" }
                    ]);
                    if kernel.has_session_key.load(Ordering::SeqCst) {
                        keys.as_array_mut().unwrap().push(
                            serde_json::json!({ "id": "key-dev", "name": syt::DEV_SESSION_CELL }),
                        );
                    }
                    return mcp_text(
                        serde_json::json!({ "action": "keys", "data": { "keys": keys } }),
                    );
                }
                if body.contains("\"database\"") && body.contains("\"item_update\"") {
                    let parsed: serde_json::Value = body.parse().unwrap_or_default();
                    let key = parsed
                        .pointer("/params/arguments/keyID")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let item = parsed
                        .pointer("/params/arguments/itemID")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let value = parsed
                        .pointer("/params/arguments/value")
                        .cloned()
                        .unwrap_or_default();
                    kernel
                        .item_updates
                        .lock()
                        .unwrap()
                        .push((key, item, value));
                    return mcp_text(
                        serde_json::json!({ "action": "item_update", "data": null }),
                    );
                }
                mcp_text(serde_json::json!({ "action": "unknown", "data": null }))
            }
        })
        .mount(server)
        .await;
}

fn probe_board() -> SyBoard {
    SyBoard {
        doc_id: "board-doc".into(),
        av_id: "board-av".into(),
        block_id: "board-block".into(),
        hpath: "/uwuwu/Задачи uwuwu".into(),
        notebook: "projects-nb".into(),
    }
}

fn probe_task(dev_session: &str) -> SyTask {
    SyTask {
        item_id: "item-1".into(),
        title: "таск".into(),
        status: "In Progress".into(),
        branch: String::new(),
        pr: String::new(),
        dev_session: dev_session.into(),
        body_doc: None,
    }
}

/// (1) Колонка есть: ячейка записана текстом sessionID в верный key/item.
#[tokio::test]
async fn claim_writes_session_into_dev_cell() {
    let kernel = Kernel::with_session_key(true);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    let sy = SiYuan::for_tests(server.uri());

    syt::claim(&sy, &probe_board(), &probe_task(""), "ses_claim1")
        .await
        .expect("заявка при наличии колонки проходит");

    let updates = kernel.updates();
    assert_eq!(updates.len(), 1, "ровно одна запись ячейки");
    let (key, item, value) = &updates[0];
    assert_eq!(key, "key-dev", "запись в колонку «Сессия dev»");
    assert_eq!(item, "item-1", "запись в строку задачи");
    assert_eq!(
        value.pointer("/text/content").and_then(|v| v.as_str()),
        Some("ses_claim1"),
        "sessionID записан text-контентом: {value}"
    );
}

/// (2) Колонки нет: громкий отказ, ноль записей (тихий no-op = агент думает,
/// что заявлен, а фабрика заспавнит дубль).
#[tokio::test]
async fn claim_bails_loudly_without_writes_when_board_lacks_session_key() {
    let kernel = Kernel::with_session_key(false);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    let sy = SiYuan::for_tests(server.uri());

    let err = syt::claim(&sy, &probe_board(), &probe_task(""), "ses_claim1")
        .await
        .expect_err("доска без колонки «Сессия dev» должна отказывать");

    assert!(
        err.to_string().contains(syt::DEV_SESSION_CELL),
        "отказ называет недостающую колонку: {err}"
    );
    assert!(
        kernel.updates().is_empty(),
        "отказ до записи: item_update не вызывался"
    );
}

/// (3) Чужая живая заявка: перезапись легитимна (перехват работы).
#[tokio::test]
async fn claim_overwrites_foreign_session_cell() {
    let kernel = Kernel::with_session_key(true);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    let sy = SiYuan::for_tests(server.uri());

    syt::claim(&sy, &probe_board(), &probe_task("ses_other0"), "ses_claim1")
        .await
        .expect("перехват заявки проходит");

    let updates = kernel.updates();
    assert_eq!(updates.len(), 1, "перезапись состоялась");
    assert_eq!(
        updates[0]
            .2
            .pointer("/text/content")
            .and_then(|v| v.as_str()),
        Some("ses_claim1"),
        "новая сессия вытеснила чужую"
    );
}
