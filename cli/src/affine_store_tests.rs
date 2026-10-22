//! Tests for affine_store: frontmatter parsing, fact section rendering,
//! table splitting, and the migration map.

use crate::affine_store::{split_frontmatter, MigrationMap};

fn fact_fixture() -> crate::affine_store::FactEntry {
    crate::affine_store::FactEntry {
        slug: "affine-port".into(),
        date: "2026-10-05".into(),
        title: "Affine port".into(),
        statement: "Selfhost listens on 3010.".into(),
        tags: vec!["affine".into()],
        confidence: "0.9".into(),
        sources: vec!["manual/2026-10-05".into()],
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
fn map_is_per_gateway_instance() {
    let dir = tempfile::tempdir().unwrap();
    // local gateway → legacy name
    let local = MigrationMap::load_for(dir.path(), "http://localhost:3000/mcp").unwrap();
    local.save().unwrap();
    assert!(dir.path().join("affine_map.json").exists());
    // remote gateway → own file, does not see the local map's entries
    let mut remote = MigrationMap::load_for(
        dir.path(),
        "https://affine-mcp-production.up.railway.app/mcp",
    )
    .unwrap();
    assert!(remote.doc_id("axum").is_none());
    remote.put_doc("axum", "prod-doc-1");
    remote.save().unwrap();
    assert!(dir.path().join("affine_map.json").exists());
    assert!(dir
        .path()
        .join("affine_map_affine-mcp-production-up-railway-app.json")
        .exists());
    // local map still knows nothing about the remote doc
    let local_again = MigrationMap::load_for(dir.path(), "http://localhost:3000/mcp").unwrap();
    assert!(local_again.doc_id("axum").is_none());
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
