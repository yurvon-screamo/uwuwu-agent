//! Интеграционные тесты MCP-транспорта против mock-шлюза из tests/common
//! (мастерская версия: hanging-шаги, Retry-After, env-пины). Ретраи:
//! UWUWU_RETRY_BASE_MS=0 — паузы бэкофа не съедают время сьюта;
//! таймаутные сценарии через UWUWU_CALL_TIMEOUT_SECS=1 и hanging-шаг.

mod common;

use common::{session_file, test_cfg, with_temp_home, MockGateway, Step, CALL_OK, INIT, NOTIFIED};
use serde_json::json;
use std::time::{Duration, Instant};
use uwuwu_cli::affine_client::AffineClient;

#[test]
#[serial_test::serial]
fn handshake_saves_session_file_and_reuse_skips_initialize() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        {
            let _client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
            assert_eq!(
                std::fs::read_to_string(session_file()).unwrap().trim(),
                "sess-A"
            );
        }
        // «процесс завершился» (лок освобождён): повторный connect на живой
        // сессии не делает НИ ОДНОГО запроса. Пока первый клиент жив, лок
        // удерживается — конкурент видит private-сессию (см. тест ниже).
        let _again = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(mock.request_count(), 2);
    });
}

#[test]
#[serial_test::serial]
fn call_tool_returns_structured_content() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
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
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, rejected),
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
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(502, None, "gateway hiccup"),
            Step::basic(502, None, "gateway hiccup again"),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 5);
    });
}

#[test]
#[serial_test::serial]
fn retry_after_header_replaces_backoff_pause() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            // 429 с Retry-After: 1 — пауза из заголовка, не из бэкофа (base=0)
            Step::with_headers(429, None, "slow down", &[("Retry-After", "1")]),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let started = Instant::now();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        // пауза пришла из Retry-After (≥0.9s), а не из base=0
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "elapsed {:?} — Retry-After не уважен",
            started.elapsed()
        );
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn stale_session_400_triggers_rehandshake_and_retry() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(400, None, "session rejected"),
            Step::basic(200, Some("sess-B"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
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
fn retryable_ok_false_is_retried_logical_ok_false_is_not() {
    with_temp_home(|| {
        let retryable = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"перегрузка","retryable":true}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, retryable),
            Step::basic(200, None, CALL_OK),
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
fn retryable_inside_text_blob_is_retried() {
    with_temp_home(|| {
        let blob = r#"{"jsonrpc":"2.0","id":{ID},"result":{"content":[{"type":"text","text":"{\"ok\": false, \"error\": \"гонка записи\", \"retryable\": true}"}]}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, blob),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("read_doc", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn empty_body_on_write_is_fatal_without_retry() {
    // живой инцидент сессии: шлюз ответил 200 с пустым телом. Запись:
    // ответ потерян, операция могла примениться — без ретраев.
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, ""),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("create_doc", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unparsable gateway response"), "got: {err}");
        assert!(err.contains("могла примениться"), "got: {err}");
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn empty_body_on_read_is_retried() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, ""),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_docs", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn rpc_error_id_mismatch_is_rejected_by_sse_parser() {
    with_temp_home(|| {
        // тело с ЧУЖИМ id — parse_response должен отвергнуть; запись —
        // Fatal без ретраев (ответ мог уйти в чужой процесс кросс-током)
        let wrong_id = r#"{"jsonrpc":"2.0","id":999,"result":{"structuredContent":{}}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, wrong_id),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("create_x", json!({})))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("id mismatch") || err.contains("unparsable"),
            "got: {err}"
        );
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn timeout_read_retries_write_fails_fast() {
    with_temp_home(|| {
        std::env::set_var("UWUWU_CALL_TIMEOUT_SECS", "1");
        // чтение: hanging-шлюз → таймаут → ретраи до исчерпания (4 попытки)
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::hanging(),
            Step::hanging(),
            Step::hanging(),
            Step::hanging(),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed after 3 retries"), "got: {err}");
        // В ЖУРНАЛЕ только первая попытка вызова: однопоточный мок спит в
        // serve(hanging), ретраи копятся в TCP-backlog без accept — их в
        // счётчике не видно. Исчерпание попыток доказывает текст ошибки.
        assert_eq!(mock.request_count(), 3, "read: init+notify+1 висящий вызов");

        // запись: тот же зависший шлюз — одна попытка, честная ошибка
        let mock_w = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::hanging(),
        ]);
        let mut wclient = rt
            .block_on(AffineClient::connect(&test_cfg(&mock_w)))
            .unwrap();
        let err = rt
            .block_on(wclient.call_tool("create_doc", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("могла примениться"), "got: {err}");
        assert_eq!(mock_w.request_count(), 3, "write: без повторов");
        std::env::remove_var("UWUWU_CALL_TIMEOUT_SECS");
    });
}

#[test]
#[serial_test::serial]
fn concurrent_connect_goes_private_and_shared_file_stays_intact() {
    with_temp_home(|| {
        // A: лок свободен → shared, публикует sess-A в файл
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            // B: лок занят → private handshake sess-P + один tools/call
            Step::basic(200, Some("sess-P"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let a = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );

        // B: лок удерживается A → private-сессия, файл не тронут
        let mut b = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A",
            "private-процесс не должен перезаписывать общий файл"
        );
        let out = rt.block_on(b.call_tool("list_tags", json!({}))).unwrap();
        assert_eq!(out["answer"], 42);
        // B ходит со СВОЕЙ сессией, не с общей
        assert!(mock.saw_header("mcp-session-id: sess-P"));
        assert_eq!(mock.request_count(), 5);
        drop(b);

        // A вышел → лок свободен → следующий старт переиспользует sess-A
        drop(a);
        let _c = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );
        assert_eq!(
            mock.request_count(),
            5,
            "C переиспользовал сессию без handshake"
        );
    });
}
