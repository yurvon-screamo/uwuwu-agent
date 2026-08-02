use std::fs;
use std::path::Path;

use uwuwu_cli::file_io::EditKind;
use uwuwu_cli::task_edit::write_task_content;
use uwuwu_cli::time_util;

fn seed_project(root: &Path, project: &str) {
    fs::create_dir_all(root.join("projects").join(project).join("tasks")).unwrap();
}

fn read_task(root: &Path, project: &str, slug: &str) -> String {
    fs::read_to_string(
        root.join("projects")
            .join(project)
            .join("tasks")
            .join(slug)
            .join("README.md"),
    )
    .unwrap()
}

#[test]
fn edit_task_creates_new_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    let outcome = write_task_content(
        "ckii",
        "new-one",
        "---\nstatus: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# New\n\nBody.",
        tmp.path(),
    )
    .unwrap();

    assert_eq!(outcome.kind, EditKind::Created);
    assert!(read_task(tmp.path(), "ckii", "new-one").contains("Body."));
}

#[test]
fn edit_task_overwrites_and_stamps_updated() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");
    write_task_content(
        "ckii",
        "t1",
        "---\nstatus: open\ncreated: 2026-01-01\nupdated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# Old\n\nOld body.",
        tmp.path(),
    )
    .unwrap();
    write_task_content(
        "ckii",
        "t1",
        "---\nstatus: in_progress\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# New\n\nFresh body.",
        tmp.path(),
    )
    .unwrap();

    let content = read_task(tmp.path(), "ckii", "t1");
    assert!(content.contains("Fresh body."));
    assert!(content.contains("status: in_progress"));
    assert!(!content.contains("Old body."));
    let today = time_util::format_date_now();
    assert!(
        content.contains(&format!("updated: {today}")),
        "updated must be stamped to today"
    );
}

#[test]
fn edit_task_rejects_empty_content() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    let err = write_task_content("ckii", "t1", "   \n  ", tmp.path()).unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[test]
fn edit_task_rejects_path_escape_in_slug() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    let err = write_task_content("ckii", "..", "x", tmp.path()).unwrap_err();
    assert!(err.to_string().contains("invalid slug"));
}

#[test]
fn edit_task_normalizes_crlf_to_lf() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    write_task_content(
        "ckii",
        "t1",
        "---\r\nstatus: open\r\ncreated: 2026-01-01\r\npriority: normal\r\ndesigned: false\r\n---\r\n\r\n# T\r\n\r\nBody line.\r\n",
        tmp.path(),
    )
    .unwrap();

    let content = read_task(tmp.path(), "ckii", "t1");
    assert!(
        !content.contains('\r'),
        "CRLF must be normalized to LF on write"
    );
    assert!(content.contains("Body line."));
}

#[test]
fn edit_task_preserves_worklogs_section_in_content() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    write_task_content(
        "ckii",
        "t1",
        "---\nstatus: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# T\n\nBody.\n## Worklogs\n\n### 2026-01-01 10:00\nprior entry\n",
        tmp.path(),
    )
    .unwrap();

    let content = read_task(tmp.path(), "ckii", "t1");
    assert!(content.contains("## Worklogs"));
    assert!(content.contains("prior entry"));
}

#[test]
fn edit_task_without_frontmatter_does_not_stamp_updated() {
    let tmp = tempfile::tempdir().unwrap();
    seed_project(tmp.path(), "ckii");

    write_task_content(
        "ckii",
        "t1",
        "# Plain body\n\nNo frontmatter here.",
        tmp.path(),
    )
    .unwrap();

    let content = read_task(tmp.path(), "ckii", "t1");
    assert!(content.contains("No frontmatter here."));
    assert!(
        !content.contains("updated:"),
        "no `updated` stamp when content has no frontmatter (mirrors file_io)"
    );
}

#[test]
fn edit_task_fails_when_project_tasks_dir_missing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("projects/ghost")).unwrap();

    let err = write_task_content("ghost", "t1", "body", tmp.path()).unwrap_err();
    assert!(err.to_string().contains("not found"));
}
