//! Юнит: person append добавляет только addition (регрессия удвоения тела
//! — ревью High 4 PR #242: cmd-слой передавал current+addition в
//! append_doc, который сам делает read+concat). Контур — живой git:
//! bare origin + рабочий клон, write_files гонит commit+push ff-only.

use crate::store::doc_store::{DocRef, DocStore};
use crate::store::github_doc_store::GitHubDocStore;
use crate::store::git_util::{git_raw, git_sync_with_author};
use anyhow::Result;
use tempfile::TempDir;

async fn setup_hub(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    let origin = dir.join(format!("{name}-origin"));
    let work = dir.join(format!("{name}-clone"));
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(&work)?;
    let origin_s = origin.to_str().unwrap();
    let work_s = work.to_str().unwrap();
    git_raw(&["init", "--bare", "-q", "-b", "main", origin_s]).await?;
    git_raw(&["-C", work_s, "init", "-q", "-b", "main"]).await?;
    git_raw(&["-C", work_s, "remote", "add", "origin", origin_s]).await?;
    let doc = work.join("docs/Experience/person/PERSON.md");
    std::fs::create_dir_all(doc.parent().unwrap())?;
    std::fs::write(&doc, "база\n")?;
    git_raw(&["-C", work_s, "add", "-A"]).await?;
    git_sync_with_author(&["-C", work_s, "commit", "-q", "-m", "seed"])?;
    git_raw(&["-C", work_s, "push", "-q", "origin", "main"]).await?;
    Ok(work)
}

#[tokio::test]
async fn person_append_adds_only_addition() -> Result<()> {
    let tmp = TempDir::new()?;
    let work = setup_hub(tmp.path(), "append").await?;
    let store = GitHubDocStore::new("local/test".into(), work.clone());
    let slug = "Experience/person/PERSON";
    store.append_doc(slug, "строка-1").await?;
    store.append_doc(slug, "строка-2").await?;
    let body = store
        .read(&DocRef {
            slug: slug.to_string(),
        })
        .await?;
    // регрессия: было "база + (база + addition)" — тело удваивалось
    assert_eq!(body, "база\nстрока-1\nстрока-2\n");
    Ok(())
}
