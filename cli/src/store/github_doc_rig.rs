//! Тестовый риг write-path docs: bare origin + рабочий клон с seed-доками
//! (общий для github_doc_write_tests и cmd-тестов). Прод-вики тестами
//! не касается.

use crate::store::github_doc_store::GitHubDocStore;
use crate::store::git_util::{git_raw, git_sync_with_author};
use anyhow::Result;
use tempfile::TempDir;

pub(crate) const ADR_DOC: &str = "Projects/proj/ADR-001: тест-решение";
pub(crate) const PLAIN_DOC: &str = "Experience/theme/plain-doc";
pub(crate) const NEW_DOC: &str = "Experience/theme/new-doc";

/// Seed-доки + git-инициализация по готовым путям (origin bare, work клон).
async fn setup_at(origin: &std::path::Path, work: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(origin)?;
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
    Ok(())
}

/// Поднять контур: bare origin + клон с seed.
pub(crate) async fn setup_hub(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    let work = dir.join(format!("{name}-clone"));
    setup_at(&dir.join(format!("{name}-origin")), &work).await?;
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

/// Чужой push мимо store-клона: отдельный clone origin, append в хвост
/// файла, commit+push. Симулирует конкурентного писателя.
pub(crate) async fn foreign_append_tail(
    dir: &std::path::Path,
    name: &str,
    rel_file: &str,
    text: &str,
) -> Result<()> {
    let w2 = dir.join(format!("{name}-foreign"));
    let origin = dir.join(format!("{name}-origin"));
    git_raw(&[
        "clone",
        "-q",
        origin.to_str().unwrap(),
        w2.to_str().unwrap(),
    ])
    .await?;
    let file = w2.join("docs").join(rel_file);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(file)?;
        f.write_all(text.as_bytes())?;
    }
    let w2_s = w2.to_str().unwrap();
    git_raw(&["-C", w2_s, "add", "-A"]).await?;
    git_sync_with_author(&["-C", w2_s, "commit", "-q", "-m", "foreign tail"])?;
    git_raw(&["-C", w2_s, "push", "-q", "origin", "main"]).await?;
    Ok(())
}

/// Env-контуры для cmd-тестов (doc_store() идёт через from_env):
/// UWUWU_HUB_CLONE_ROOT/HUB_REPO в tmp, клон с seed по конвенции
/// <repo-slashes→__>. Тест с env — #[serial] (гонка env-переменных).
pub(crate) async fn hub_env(tmp: &TempDir) -> Result<std::path::PathBuf> {
    std::env::set_var("UWUWU_HUB_CLONE_ROOT", tmp.path());
    std::env::set_var("UWUWU_HUB_REPO", "local/test");
    let work = tmp.path().join("local__test");
    setup_at(&tmp.path().join("local__test-origin"), &work).await?;
    Ok(work)
}
