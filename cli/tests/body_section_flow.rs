//! Сценарные тесты replace_body_section против mock-шлюза: порядок вызовов
//! (read → delete × N → append → read для конвертации фенсов), уход
//! placement в append_markdown и поведение при ok:false в середине серии.

use serde_json::Value;
use uwuwu_cli::affine_body::replace_body_section;
use uwuwu_cli::affine_client::AffineClient;

mod common;
use common::{test_cfg, with_temp_home, MockGateway, Step, INIT, NOTIFIED};

const READ_DOC: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"docId":"body1","blocks":[
 {"id":"note","flavour":"affine:note","childIds":["h2x","p1"]},
 {"id":"h2x","flavour":"affine:paragraph","type":"h2","text":"Решение"},
 {"id":"p1","flavour":"affine:paragraph","type":"text","text":"старый текст"}]}}}"#;
const DELETE_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"deleted":true}}}"#;
const APPEND_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"appended":true}}}"#;
const DELETE_FAIL: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":false,"error":"blocked"}}}"#;
const APPEND_FAIL: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":false,"error":"disk full"}}}"#;

/// Имя tools/call из тела запроса (None для initialize/notifications).
fn tool_name(req: &Value) -> Option<&str> {
    req.pointer("/params/name").and_then(|n| n.as_str())
}

fn run_replace(
    mock_steps: Vec<Step>,
    md: &str,
) -> (Result<(usize, usize), anyhow::Error>, MockGateway) {
    let mock = MockGateway::start(mock_steps);
    let cfg = test_cfg(&mock);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        let mut client = AffineClient::connect(&cfg).await.unwrap();
        replace_body_section(&mut client, &cfg, "body1", "Решение", md).await
    });
    (result, mock)
}

fn base_steps() -> Vec<Step> {
    vec![Step(200, Some("sess-A"), INIT), Step(202, None, NOTIFIED)]
}

#[test]
#[serial_test::serial]
fn replace_flow_calls_read_delete_append_with_placement_after_heading() {
    with_temp_home(|| {
        // Arrange: шаги = init, notified, read_doc, delete, append, read_doc(фенсы)
        let mut steps = base_steps();
        steps.push(Step(200, None, READ_DOC));
        steps.push(Step(200, None, DELETE_OK));
        steps.push(Step(200, None, APPEND_OK));
        steps.push(Step(200, None, READ_DOC));

        // Act
        let (result, mock) = run_replace(steps, "новый текст");

        // Assert: замена прошла, контент заменился без диаграмм
        assert_eq!(result.unwrap(), (1, 0));
        let calls: Vec<Value> = mock
            .request_bodies()
            .iter()
            .filter_map(|raw| serde_json::from_str(raw.split("\r\n\r\n").nth(1)?).ok())
            .filter(|r| tool_name(r).is_some())
            .collect();
        let names: Vec<&str> = calls.iter().filter_map(|r| tool_name(r)).collect();
        assert_eq!(
            names,
            vec!["read_doc", "delete_block", "append_markdown", "read_doc"]
        );
        // delete целялся в блок контента секции
        assert_eq!(calls[1].pointer("/params/arguments/blockId").unwrap(), "p1");
        // append встаёт сразу после заголовка секции, markdown передан
        let append = &calls[2];
        assert_eq!(
            append
                .pointer("/params/arguments/placement/afterBlockId")
                .unwrap(),
            "h2x"
        );
        assert_eq!(
            append.pointer("/params/arguments/markdown").unwrap(),
            "новый текст"
        );
    });
}

#[test]
#[serial_test::serial]
fn delete_ok_false_aborts_before_append() {
    with_temp_home(|| {
        // Arrange: первый же delete отвечает логическим отказом
        let mut steps = base_steps();
        steps.push(Step(200, None, READ_DOC));
        steps.push(Step(200, None, DELETE_FAIL));

        // Act
        let (result, mock) = run_replace(steps, "новый текст");

        // Assert: честная ошибка, append не состоялся
        let err = result.unwrap_err().to_string();
        assert!(err.contains("blocked"), "got: {err}");
        assert!(err.contains("0 блоков"), "got: {err}");
        let has_append = mock
            .request_bodies()
            .iter()
            .any(|raw| raw.contains("\"name\":\"append_markdown\""));
        assert!(!has_append);
    });
}

#[test]
#[serial_test::serial]
fn append_ok_false_error_reports_cleared_section_without_insert() {
    with_temp_home(|| {
        // Arrange: удаления прошли, append отвечает логическим отказом
        let mut steps = base_steps();
        steps.push(Step(200, None, READ_DOC));
        steps.push(Step(200, None, DELETE_OK));
        steps.push(Step(200, None, APPEND_FAIL));

        // Act
        let (result, mock) = run_replace(steps, "новый текст");

        // Assert: ошибка говорит, что секция уже пуста и контент не вставлен;
        // конвертация фенсов (второй read_doc) не выполнялась
        let err = result.unwrap_err().to_string();
        assert!(err.contains("секция уже очищена"), "got: {err}");
        assert!(err.contains("disk full"), "got: {err}");
        let read_count = mock
            .request_bodies()
            .iter()
            .filter(|raw| raw.contains("\"name\":\"read_doc\""))
            .count();
        assert_eq!(read_count, 1);
    });
}

#[test]
#[serial_test::serial]
fn empty_markdown_clears_section_without_append() {
    with_temp_home(|| {
        // Arrange: пустой replacement — только чтение и удаление
        let mut steps = base_steps();
        steps.push(Step(200, None, READ_DOC));
        steps.push(Step(200, None, DELETE_OK));

        // Act
        let (result, mock) = run_replace(steps, "   \n");

        // Assert: секция очищена, append не вызывался
        assert_eq!(result.unwrap(), (1, 0));
        let has_append = mock
            .request_bodies()
            .iter()
            .any(|raw| raw.contains("\"name\":\"append_markdown\""));
        assert!(!has_append);
    });
}
