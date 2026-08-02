use std::fs;
use std::path::Path;

use uwuwu_cli::projects::collect_projects;
use uwuwu_cli::task_list::{collect_groups, ListArgs};
use uwuwu_cli::task_search::{run_search, SearchArgs};

fn write(root: &Path, relpath: &str, body: &str) {
    let full = root.join(relpath);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

fn write_task(root: &Path, project: &str, slug: &str, fm: &str, body: &str) {
    let path = root
        .join("projects")
        .join(project)
        .join("tasks")
        .join(slug)
        .join("README.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = format!("---\n{fm}---\n\n{body}\n");
    fs::write(path, content).unwrap();
}

#[test]
fn collect_projects_lists_dirs_with_counts_and_skips_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_task(
        root,
        "alpha",
        "t1",
        "status: open\ncreated: 2026-01-01\n",
        "body",
    );
    write_task(
        root,
        "alpha",
        "t2",
        "status: open\ncreated: 2026-01-02\n",
        "body",
    );
    write(root, "projects/alpha/access/a1.md", "body");
    write(
        root,
        "projects/alpha/README.md",
        "# Alpha\n\nA project description.",
    );
    write(root, "projects/stray.txt", "not a project");

    let entries = collect_projects(root).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.name, "alpha");
    assert_eq!(e.tasks_count, 2);
    assert_eq!(e.access_count, 1);
    assert_eq!(e.description, "A project description.");
}

#[test]
fn collect_projects_empty_when_no_projects_dir() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(collect_projects(tmp.path()).unwrap().is_empty());
}

#[test]
fn collect_groups_single_project_status_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_task(
        root,
        "p",
        "open-task",
        "status: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\ntitle: Open\n",
        "body",
    );
    write_task(
        root,
        "p",
        "closed-task",
        "status: closed\ncreated: 2026-01-02\npriority: normal\ndesigned: false\ntitle: Closed\n",
        "body",
    );

    let args = ListArgs {
        project: Some("p".to_string()),
        status: Some("open".to_string()),
        from: None,
        to: None,
        priority: None,
        deadline_from: None,
        deadline_to: None,
        designed: None,
    };
    let groups = collect_groups(&args, root).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, "p");
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].slug, "open-task");
    assert_eq!(groups[0].1[0].title.as_deref(), Some("Open"));
}

#[test]
fn collect_groups_omits_closed_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_task(
        root,
        "p",
        "open-task",
        "status: open\ncreated: 2026-01-01\n",
        "body",
    );
    write_task(
        root,
        "p",
        "closed-task",
        "status: closed\ncreated: 2026-01-02\n",
        "body",
    );

    let args = ListArgs {
        project: Some("p".to_string()),
        status: None,
        from: None,
        to: None,
        priority: None,
        deadline_from: None,
        deadline_to: None,
        designed: None,
    };
    let groups = collect_groups(&args, root).unwrap();
    let slugs: Vec<&str> = groups[0].1.iter().map(|e| e.slug.as_str()).collect();
    assert_eq!(slugs, vec!["open-task"]);
}

#[test]
fn collect_groups_missing_tasks_dir_returns_empty_group() {
    let tmp = tempfile::tempdir().unwrap();
    let args = ListArgs {
        project: Some("ghost".to_string()),
        status: None,
        from: None,
        to: None,
        priority: None,
        deadline_from: None,
        deadline_to: None,
        designed: None,
    };
    let groups = collect_groups(&args, tmp.path()).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, "ghost");
    assert!(groups[0].1.is_empty());
}

#[test]
fn collect_groups_multi_project_groups_and_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_task(
        root,
        "zeta",
        "z1",
        "status: open\ncreated: 2026-01-01\n",
        "body",
    );
    write_task(
        root,
        "alpha",
        "a1",
        "status: open\ncreated: 2026-01-01\n",
        "body",
    );

    let args = ListArgs {
        project: None,
        status: None,
        from: None,
        to: None,
        priority: None,
        deadline_from: None,
        deadline_to: None,
        designed: None,
    };
    let groups = collect_groups(&args, root).unwrap();
    let names: Vec<&str> = groups.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zeta"]);
}

#[tokio::test]
async fn run_search_rejects_inverted_date_range_without_calling_ollama() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let args = SearchArgs {
        project: "p".to_string(),
        query: "anything".to_string(),
        top: 3,
        status: None,
        from: Some("2026-02-01".to_string()),
        to: Some("2026-01-01".to_string()),
        priority: None,
        deadline_from: None,
        deadline_to: None,
        designed: None,
    };
    let outcome = run_search(&args, root, &root.join("cache.db")).await;
    match outcome {
        Ok(_) => panic!("expected an error for inverted date range"),
        Err(err) => assert!(err.to_string().contains("invalid date range")),
    }
}
