//! Интеграционные тесты MCP-транспорта против mock-шлюза из tests/common.

mod common;

use common::{session_file, test_cfg, with_temp_home, MockGateway, Step, CALL_OK, INIT, NOTIFIED};
use serde_json::json;
use uwuwu_cli::affine_client::AffineClient;

#[test]
#[serial_test::serial]
fn handshake_saves_session_file_and_reuse_skips_initialize() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );
        // повторный connect на живой сессии не делает НИ ОДНОГО запроса
        let _again = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(mock.request_count(), 2);
    });
}

#[test]
#[serial_test::serial]
fn call_tool_returns_structured_content() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("search_docs", json!({"q": "x"})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn gateway_ok_false_convention_fails_fast_without_retry() {
    with_temp_home(|| {
        let rejected = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"инцидент"}}"#;
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, rejected),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("add_database_row", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("инцидент"), "got: {err}");
        // логический фейл — Fatal: ни одного повторного запроса
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn transport_5xx_is_retried_and_then_succeeds() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(500, None, "gateway hiccup"),
            Step(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn stale_session_400_triggers_rehandshake_and_retry() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(400, None, "session rejected"),
            Step(200, Some("sess-B"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("read_rows", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        // пере-handshake перезаписал общий файл новой сессией
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-B"
        );
        assert_eq!(mock.request_count(), 6);
    });
}

#[test]
#[serial_test::serial]
fn empty_body_is_reported_as_unparsable_not_panicked() {
    // живой инцидент сессии: шлюз ответил 200 с пустым телом
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, ""),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("anything", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unparsable gateway response"), "got: {err}");
    });
}

#[test]
#[serial_test::serial]
fn rpc_error_id_mismatch_is_rejected_by_sse_parser() {
    with_temp_home(|| {
        // тело с ЧУЖИМ id — parse_response должен отвергнуть
        let wrong_id = r#"{"jsonrpc":"2.0","id":999,"result":{"structuredContent":{}}}"#;
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, wrong_id),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("x", json!({})))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("id mismatch") || err.contains("unparsable"),
            "got: {err}"
        );
    });
}
