//! Чистые пины предпроверки версии удаления (#1697): четыре вердикта
//! `check_expected_blob` без сети и HTTP-моков (ADR-110).

use super::{check_expected_blob, ExpectedBlob};
use serde_json::json;
use uwuwu_usecase::UsecaseError;

fn tree(entries: serde_json::Value) -> serde_json::Value {
    json!({ "tree": entries, "truncated": false })
}

#[test]
fn precheck_accepts_matching_blob_sha() {
    let current = tree(json!([
        {"path": "docs/Projects/foo.md", "type": "blob", "sha": "A"}
    ]));
    let expected = ExpectedBlob {
        path: "docs/Projects/foo.md",
        sha: "A",
    };
    assert!(check_expected_blob(&current, &expected).is_ok());
}

#[test]
fn precheck_refuses_changed_blob_sha_with_race_ref() {
    let current = tree(json!([
        {"path": "docs/Projects/foo.md", "type": "blob", "sha": "B"}
    ]));
    let expected = ExpectedBlob {
        path: "docs/Projects/foo.md",
        sha: "A",
    };
    let error = check_expected_blob(&current, &expected).expect_err("расхождение");
    assert!(matches!(error, UsecaseError::RaceRef(_)), "{error}");
    assert!(error.to_string().contains('B'), "{error}");
}

#[test]
fn precheck_reports_already_absent_without_backup_denial() {
    let current = tree(json!([
        {"path": "docs/other.md", "type": "blob", "sha": "A"}
    ]));
    let expected = ExpectedBlob {
        path: "docs/Projects/foo.md",
        sha: "A",
    };
    let error = check_expected_blob(&current, &expected).expect_err("уже удалён");
    assert!(matches!(error, UsecaseError::DocAbsentAtRef(_)), "{error}");
    let text = error.to_string();
    assert!(!text.contains("no backup"), "{text}");
    assert!(text.contains("backup of the read version only"), "{text}");
}

#[test]
fn precheck_refuses_truncated_tree() {
    let current = json!({"tree": [], "truncated": true});
    let expected = ExpectedBlob {
        path: "docs/Projects/foo.md",
        sha: "A",
    };
    let error = check_expected_blob(&current, &expected).expect_err("усечённое дерево");
    assert!(matches!(error, UsecaseError::PrecheckUnavailable(_)), "{error}");
}

#[test]
fn precheck_refuses_tree_without_entries_array() {
    let expected = ExpectedBlob {
        path: "docs/Projects/foo.md",
        sha: "A",
    };
    let error = check_expected_blob(&json!({}), &expected).expect_err("битый ответ");
    assert!(matches!(error, UsecaseError::PrecheckUnavailable(_)), "{error}");
}
