use std::path::Path;

use uwuwu_cli::request::{create_request, delete_request, get_request, list_requests};

fn make_wiki(dir: &Path) {
    std::fs::create_dir_all(dir.join("experience")).unwrap();
}

fn dir_of(dir: &Path) -> std::path::PathBuf {
    dir.join(".requests")
}

#[test]
fn create_then_list_and_get_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    make_wiki(tmp.path());
    let dir = dir_of(tmp.path());

    let path = create_request(
        "create",
        "databases/redis.md",
        "---\ntitle: Redis\n---\n\n# Redis\n\nBody.",
        "add redis notes",
        &dir,
    )
    .unwrap();
    assert!(path.starts_with(&dir));

    let listed = list_requests(&dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].action, "create");
    assert_eq!(listed[0].target, "databases/redis.md");
    assert_eq!(listed[0].reason, "add redis notes");

    let id = &listed[0].id;
    let content = get_request(id, &dir).unwrap();
    assert!(content.contains("type: create"));
    assert!(content.contains("target: databases/redis.md"));
    assert!(content.contains("Body."));
}

#[test]
fn delete_discards_request() {
    let tmp = tempfile::tempdir().unwrap();
    make_wiki(tmp.path());
    let dir = dir_of(tmp.path());

    create_request("create", "x.md", "body", "r", &dir).unwrap();
    let id = list_requests(&dir).unwrap()[0].id.clone();

    delete_request(&id, &dir).unwrap();
    assert!(list_requests(&dir).unwrap().is_empty());
}

#[test]
fn resolve_rejects_path_escape_in_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = dir_of(tmp.path());

    let err = get_request("../outside", &dir).unwrap_err();
    assert!(err.to_string().contains("invalid"));
}

#[test]
fn list_empty_when_no_requests_dir() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(list_requests(&dir_of(tmp.path())).unwrap().is_empty());
}

#[test]
fn id_prefix_match_resolves_unique_request() {
    let tmp = tempfile::tempdir().unwrap();
    make_wiki(tmp.path());
    let dir = dir_of(tmp.path());

    create_request("create", "alpha.md", "body", "r", &dir).unwrap();
    let full_id = list_requests(&dir).unwrap()[0].id.clone();
    let prefix: String = full_id.chars().take(8).collect();

    let content = get_request(&prefix, &dir).unwrap();
    assert!(content.contains("target: alpha.md"));
}

#[test]
fn ambiguous_id_prefix_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    make_wiki(tmp.path());
    let dir = dir_of(tmp.path());

    create_request("create", "alpha.md", "body", "r", &dir).unwrap();
    create_request("create", "beta.md", "body", "r", &dir).unwrap();
    let ids: Vec<String> = list_requests(&dir)
        .unwrap()
        .iter()
        .map(|e| e.id.clone())
        .collect();
    let common = common_prefix(&ids[0], &ids[1]);
    assert!(common.len() >= 4, "ids should share a timestamp prefix");

    let err = get_request(&common, &dir).unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
}

#[test]
fn create_rejects_empty_content_for_create_action() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = dir_of(tmp.path());

    let err = create_request("create", "x.md", "   ", "r", &dir).unwrap_err();
    assert!(err.to_string().contains("content is required"));
}

#[test]
fn create_allows_empty_content_for_delete_action() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = dir_of(tmp.path());

    create_request("delete", "obsolete.md", "", "gone", &dir).unwrap();
    let listed = list_requests(&dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].action, "delete");
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect()
}
