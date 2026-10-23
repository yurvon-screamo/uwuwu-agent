//! Tests for affine_store: frontmatter parsing, fact section rendering,
//! table splitting, and the migration map.

use crate::affine_client::AffineConfig;
use crate::affine_store::{split_frontmatter, MigrationMap};

fn cfg(url: &str, workspace: &str) -> AffineConfig {
    AffineConfig {
        url: url.to_string(),
        token: "t".to_string(),
        workspace: workspace.to_string(),
    }
}

fn fact_fixture() -> crate::affine_store::FactEntry {
    crate::affine_store::FactEntry {
        slug: "affine-port".into(),
        date: "2026-10-05".into(),
        title: "Affine port".into(),
        statement: "Selfhost listens on 3010.".into(),
        tags: vec!["affine".into()],
        confidence: "0.9".into(),
        sources: vec!["manual/2026-10-05".into()],
        status: "accepted".into(),
    }
}

#[test]
fn frontmatter_roundtrip_fact_file() {
    let raw = "---\nupdated: 2026-10-05\ntitle: Port 3010 affine\ndescription: d\nconfidence: 0.9\nsources: [manual/2026-10-05, opencode/x]\ntags: [affine, docker]\nstatus: accepted\n---\n\nAffine selfhost listens on 3010.";
    let (fm, body) = split_frontmatter(raw);
    assert_eq!(fm.updated.as_deref(), Some("2026-10-05"));
    assert_eq!(fm.tags, vec!["affine", "docker"]);
    assert_eq!(fm.sources.len(), 2);
    assert_eq!(fm.confidence.as_deref(), Some("0.9"));
    assert!(body.starts_with("Affine selfhost"));
}

#[test]
fn frontmatter_absent_yields_whole_body() {
    let (fm, body) = split_frontmatter("just text");
    assert!(fm.title.is_none());
    assert_eq!(body, "just text");
}

#[test]
fn early_horizontal_rule_is_not_frontmatter() {
    let raw = "# Title\n\nSome text\n\n---\n\nafter the rule";
    let (fm, body) = split_frontmatter(raw);
    assert!(fm.updated.is_none());
    assert!(body.starts_with("# Title"));
    assert!(body.contains("after the rule"));
}

#[test]
fn crlf_frontmatter_is_normalized() {
    let raw = "---\r\ntitle: CRLF doc\r\n---\r\n\r\nBody here";
    let (fm, body) = split_frontmatter(raw);
    assert_eq!(fm.title.as_deref(), Some("CRLF doc"));
    assert_eq!(body, "Body here");
}

#[test]
fn fact_section_contains_meta_and_statement() {
    let section = crate::affine_store::render_fact_section(&fact_fixture());
    assert!(section.contains("## Affine port"));
    assert!(section.contains("Selfhost listens on 3010."));
    assert!(section.contains("slug: affine-port"));
    assert!(section.contains("conf 0.9"));
}

#[test]
fn map_is_per_gateway_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let remote = cfg(
        "https://affine-mcp-production.up.railway.app/mcp",
        "81475705-028a-4153-a8f5-f06e402ee31e",
    );
    let mut ws_a = MigrationMap::load_for(dir.path(), &remote).unwrap();
    ws_a.put_doc("axum", "doc-a");
    ws_a.save().unwrap();
    // same gateway, another workspace → another file, no shared docIds
    let ws_b = cfg(&remote.url, "00000000-dead-beef-0000-000000000000");
    let map_b = MigrationMap::load_for(dir.path(), &ws_b).unwrap();
    assert!(map_b.doc_id("axum").is_none());
    // reload of the first workspace still sees its entry
    let ws_a_again = MigrationMap::load_for(dir.path(), &remote).unwrap();
    assert_eq!(ws_a_again.doc_id("axum"), Some("doc-a"));
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("affine_map_"))
        .collect();
    assert_eq!(
        files.len(),
        1,
        "one file per instance+workspace, created lazily"
    );
}

#[test]
fn migration_map_roundtrip_and_corruption_is_loud() {
    let dir = tempfile::tempdir().unwrap();
    let mut map = MigrationMap::load(dir.path()).unwrap();
    assert!(map.doc_id("axum").is_none());
    map.put_doc("axum", "doc1");
    map.put_container("Experience", "c1");
    map.put_daily_bucket("2026-10-23", "d1");
    map.save().unwrap();

    let reloaded = MigrationMap::load(dir.path()).unwrap();
    assert_eq!(reloaded.doc_id("axum"), Some("doc1"));
    assert_eq!(reloaded.container("Experience"), Some("c1"));
    assert_eq!(reloaded.daily_bucket("2026-10-23"), Some("d1"));

    std::fs::write(dir.path().join("affine_map.json"), "{not json").unwrap();
    assert!(MigrationMap::load(dir.path()).is_err());
}

#[test]
fn map_save_is_atomic_no_tmp_leftover() {
    let dir = tempfile::tempdir().unwrap();
    let map = MigrationMap::load(dir.path()).unwrap();
    map.save().unwrap();
    assert!(dir.path().join("affine_map.json").exists());
    assert!(!dir.path().join("affine_map.json.tmp").exists());
}
