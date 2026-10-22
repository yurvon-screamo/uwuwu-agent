//! Tests for the SSE/JSON-RPC response parser.

use crate::affine_sse::parse_response;

#[test]
fn plain_json_body_matches_expected_id() {
    let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
    let value = parse_response(raw, 1).unwrap();
    assert!(value["result"]["ok"].as_bool().unwrap());
}

#[test]
fn plain_json_body_with_wrong_id_is_rejected() {
    let raw = r#"{"jsonrpc":"2.0","id":9,"result":{}}"#;
    assert!(parse_response(raw, 1).is_err());
}

#[test]
fn sse_event_matches_id_and_skips_foreign_events() {
    let raw = "event: message\ndata: {\"id\":7,\"x\":1}\n\nevent: message\ndata: {\"id\":3,\"result\":{}}\n\n";
    let value = parse_response(raw, 3).unwrap();
    assert!(value["result"].is_object());
}

#[test]
fn sse_multiline_data_concatenates_with_newline() {
    let raw = "data: {\"id\":5,\ndata: \"result\":{\"ok\":true}}\n\n";
    let value = parse_response(raw, 5).unwrap();
    assert!(value["result"]["ok"].as_bool().unwrap());
}

#[test]
fn data_prefix_without_space_is_accepted() {
    let raw = "data:{\"id\":2,\"result\":1}\n\n";
    assert!(parse_response(raw, 2).is_ok());
}

#[test]
fn empty_body_is_error() {
    assert!(parse_response("", 1).is_err());
    assert!(parse_response("   ", 1).is_err());
}
