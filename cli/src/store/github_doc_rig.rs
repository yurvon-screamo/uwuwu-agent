//! Тестовый риг write-path docs: bare origin + рабочий клон с seed-доками
//! (общий для github_doc_write_tests). Прод-вики тестами не касается.

use crate::store::github_doc_store::GitHubDocStore;
use crate::store::git_util::{git_raw, git_sync_with_author};
use anyhow::Result;
use tempfile::TempDir;

pub(crate) const ADR_DOC: &str = "Projects/proj/ADR-001: тест-решение";
pub(crate) const PLAIN_DOC: &str = "Experience/theme/plain-doc";
pub(crate) const NEW_DOC: &str = "Experience/theme/new-doc";

/// Поднять контур: bare origin + клон с seed (ADR-001 + plain-doc).
pub(crate) async fn setup_hub(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    let origin = dir.join(format!("{name}-origin"));
    let work = dir.join(format!("{name}-clone"));
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(work.join("docs/Projects/proj"))?;
    std::fs::create_dir_all(work.join("docs/Experience/theme"))?;
    let origin_s = origin.to_str().unwrap();
    let work_s = work.to_str().unwrap();
    git_raw(&["init", "--bare", "-q", "-b", "main", origin_s]).await?;
    git_raw(&["-C", work_s, "init", "-q", "-b", "main"]).await?;
    git_raw(&["-C", work_s, "remote", "add", "origin", origin_s]).await?;
    std::fs::write(
        work.join("docs/Projects/proj/ADR-001: тест-решение.md"),
        "---\ntitle: ADR-001: тест-решение\n---\n\n# ADR-001: тест-решение\n\nтело\n",
    )?;
    std::fs::write(
        work.join("docs/Experience/theme/plain-doc.md"),
        "---\ntitle: plain-doc\n---\n\n# plain-doc\n\nтело\n",
    )?;
    git_raw(&["-C", work_s, "add", "-A"]).await?;
    git_sync_with_author(&["-C", work_s, "commit", "-q", "-m", "seed"])?;
    git_raw(&["-C", work_s, "push", "-q", "origin", "main"]).await?;
    Ok(work)
}

pub(crate) fn hub(work: &std::path::Path) -> GitHubDocStore {
    GitHubDocStore::new("local/test".into(), work.to_path_buf())
}

/// Утилита тестов: свежий контур в tmp.
pub(crate) async fn rig(name: &str) -> Result<(TempDir, std::path::PathBuf)> {
    let tmp = TempDir::new()?;
    let work = setup_hub(tmp.path(), name).await?;
    Ok((tmp, work))
}
