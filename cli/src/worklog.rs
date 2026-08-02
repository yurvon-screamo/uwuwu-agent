use std::path::Path;

use anyhow::{Context, Result};

use crate::path as safe_path;
use crate::time_util;

pub fn apply_worklog(project: &str, slug: &str, text: &str, wiki_root: &Path) -> Result<()> {
    let canonical = safe_path::resolve_task_readme(project, slug, wiki_root)?;

    if text.trim().is_empty() {
        anyhow::bail!("worklog text is empty");
    }

    let content = std::fs::read_to_string(&canonical)
        .with_context(|| format!("cannot read: {project}/{slug}"))?;

    let updated = append_entry(&content, text);

    let canonical_projects = wiki_root
        .join("projects")
        .canonicalize()
        .with_context(|| "projects root not found")?;
    let prepared = crate::file_io::normalize_line_endings(&updated);
    crate::file_io::atomic_write(&canonical, prepared.as_bytes(), &canonical_projects)?;

    Ok(())
}

pub fn append(project: &str, slug: &str, text: &str, wiki_root: &Path) -> Result<()> {
    apply_worklog(project, slug, text, wiki_root)?;
    println!("Worklog appended to: {project}/{slug}");
    Ok(())
}

fn append_entry(content: &str, text: &str) -> String {
    let timestamp = time_util::format_timestamp_now();
    let entry = format!("### {timestamp}\n{text}\n");
    let separator = separator_for_append(content);

    if !has_worklogs_section(content) {
        return format!("{content}{separator}## Worklogs\n\n{entry}");
    }

    format!("{content}{separator}{entry}")
}

fn has_worklogs_section(content: &str) -> bool {
    content.lines().any(|line| line.trim() == "## Worklogs")
}

fn separator_for_append(content: &str) -> &'static str {
    if content.is_empty() || content.ends_with("\n\n") {
        ""
    } else if content.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_task(root: &Path, project: &str, slug: &str, body: &str) {
        let dir = root.join("projects").join(project).join("tasks").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), body).unwrap();
    }

    #[test]
    fn apply_worklog_creates_worklogs_section_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        write_task(
            tmp.path(),
            "ckii",
            "t1",
            "---\nstatus: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# Task\n\nBody.\n",
        );

        apply_worklog("ckii", "t1", "did the thing", tmp.path()).unwrap();

        let path = tmp.path().join("projects/ckii/tasks/t1/README.md");
        let updated = fs::read_to_string(path).unwrap();
        assert!(updated.contains("## Worklogs"));
        assert!(updated.contains("did the thing"));
    }

    #[test]
    fn apply_worklog_appends_to_existing_worklogs_section() {
        let tmp = tempfile::tempdir().unwrap();
        write_task(
            tmp.path(),
            "ckii",
            "t1",
            "---\nstatus: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\n# Task\n\n## Worklogs\n\n### 2026-01-01 10:00\nfirst\n",
        );

        apply_worklog("ckii", "t1", "second entry", tmp.path()).unwrap();

        let updated =
            fs::read_to_string(tmp.path().join("projects/ckii/tasks/t1/README.md")).unwrap();
        assert_eq!(
            updated.matches("## Worklogs").count(),
            1,
            "section must not be duplicated"
        );
        assert!(updated.contains("first"));
        assert!(updated.contains("second entry"));
    }

    #[test]
    fn apply_worklog_rejects_empty_text() {
        let tmp = tempfile::tempdir().unwrap();
        write_task(
            tmp.path(),
            "ckii",
            "t1",
            "---\nstatus: open\ncreated: 2026-01-01\npriority: normal\ndesigned: false\n---\n\nbody\n",
        );

        let err = apply_worklog("ckii", "t1", "   ", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }
}
