//! Tests for affine_collect: FS collectors and date grouping.

use crate::affine_collect::{collect_experience, collect_facts, collect_projects, group_by_date};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn experience_nested_slug_keeps_subdir() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("experience/cicd/gh-actions.md"),
        "---\ntitle: GH Actions\n---\n\nBody",
    );
    let result = collect_experience(dir.path());
    assert!(result.errors.is_empty());
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].slug, "cicd/gh-actions");
    assert_eq!(result.items[0].title, "GH Actions");
}

#[test]
fn unreadable_file_is_reported_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("experience/article.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"\xff\xfe not utf8").unwrap();
    let result = collect_experience(dir.path());
    assert_eq!(result.items.len(), 0);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("article.md"));
}

#[test]
fn facts_group_by_updated_date_with_month_fallback() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("facts/2026-06/a.md"),
        "---\nupdated: 2026-06-02\ntitle: A\n---\n\nStatement A",
    );
    write(
        &dir.path().join("facts/2026-06/b.md"),
        "---\ntitle: B\n---\n\nStatement B",
    );
    let result = collect_facts(dir.path());
    assert!(result.errors.is_empty());
    let grouped = group_by_date(&result.items);
    let dates: Vec<&String> = grouped.keys().collect();
    assert_eq!(dates, vec!["2026-06-01", "2026-06-02"]);
}

#[test]
fn projects_skip_readme_but_use_it_as_container_body() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("projects/origa/README.md"),
        "# Origa readme",
    );
    write(
        &dir.path().join("projects/origa/topology.md"),
        "---\ntitle: Topology\n---\n\nBody",
    );
    let result = collect_projects(dir.path());
    assert!(result.errors.is_empty());
    assert_eq!(result.items.len(), 1);
    let project = &result.items[0];
    assert_eq!(project.name, "origa");
    assert!(project.readme.contains("Origa readme"));
    assert_eq!(project.docs.len(), 1);
    assert_eq!(project.docs[0].slug, "origa/topology");
}

#[test]
fn missing_readme_falls_back_to_project_title() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("projects/bare/runbook.md"), "# Runbook");
    let result = collect_projects(dir.path());
    let project = &result.items[0];
    assert!(project.readme.contains("# bare"));
    assert_eq!(project.docs.len(), 1);
}
