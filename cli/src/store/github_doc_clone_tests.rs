//! Юнит: person append добавляет только addition (регрессия удвоения тела
//! — ревью High 4 PR #242: cmd-слой передавал current+addition в
//! append_doc, который сам делает read+concat). Контур — живой git:
//! bare origin + рабочий клон, write_files гонит commit+push ff-only.

use crate::store::doc_store::{DocEntry, DocRef, DocStore};
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

// docs ls отказ на неизвестном пути: help обещает раскладку доступных
// пространств, не сырую os error (задача 1094). Seed — три топ-папки +
// два проекта + файл-док для кейсов «файл»/«файл с хвостом».
async fn setup_ls(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    let origin = dir.join(format!("{name}-origin"));
    let work = dir.join(format!("{name}-clone"));
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(&work)?;
    let origin_s = origin.to_str().unwrap();
    let work_s = work.to_str().unwrap();
    git_raw(&["init", "--bare", "-q", "-b", "main", origin_s]).await?;
    git_raw(&["-C", work_s, "init", "-q", "-b", "main"]).await?;
    git_raw(&["-C", work_s, "remote", "add", "origin", origin_s]).await?;
    for rel in [
        "docs/Experience/person/PERSON.md",
        "docs/Projects/uwuwu/ADR/ADR-001: тест.md",
        "docs/Projects/uwuwu/Деплоймент.md",
        "docs/Projects/origa/README.md",
        "docs/Daily/2026-11-14.md",
    ] {
        let f = work.join(rel);
        std::fs::create_dir_all(f.parent().unwrap())?;
        std::fs::write(&f, "# seed\n")?;
    }
    git_raw(&["-C", work_s, "add", "-A"]).await?;
    git_sync_with_author(&["-C", work_s, "commit", "-q", "-m", "seed"])?;
    git_raw(&["-C", work_s, "push", "-q", "origin", "main"]).await?;
    Ok(work)
}

async fn ls_store(dir: &std::path::Path, name: &str) -> Result<GitHubDocStore> {
    let work = setup_ls(dir, name).await?;
    Ok(GitHubDocStore::new("local/test".into(), work))
}

#[tokio::test]
async fn ls_unknown_top_level_errors_with_space_layout() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-top").await?;
    let err = store.ls("nosuchplace123").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("nosuchplace123"), "msg: {msg}");
    assert!(msg.contains("доступн"), "msg: {msg}");
    // перечень детей docs/ — сортированный вывод ls, папки с «/»
    assert!(msg.contains("Daily/, Experience/, Projects/"), "msg: {msg}");
    assert!(!msg.contains("os error"), "msg: {msg}");
    Ok(())
}

#[tokio::test]
async fn ls_unknown_folder_child_errors_with_sibling_layout() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-child").await?;
    let err = store.ls("Projects/nosuch").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Projects/nosuch"), "msg: {msg}");
    // раскладка — дети существующего предка (Projects), не топ-папки
    assert!(msg.contains("origa/, uwuwu/"), "msg: {msg}");
    assert!(!msg.contains("os error"), "msg: {msg}");
    Ok(())
}

#[tokio::test]
async fn ls_of_file_hints_read_with_slug() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-file").await?;
    let err = store.ls("Projects/uwuwu/Деплоймент.md").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("файл"), "msg: {msg}");
    // слаг для read: путь в docs/ без «.md»
    assert!(
        msg.contains("read \"Projects/uwuwu/Деплоймент\""),
        "msg: {msg}"
    );
    assert!(!msg.contains("os error"), "msg: {msg}");
    Ok(())
}

#[tokio::test]
async fn ls_below_file_hints_read_of_file_ancestor() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-tail").await?;
    // «файл/хвост»: ближайший существующий предок — сам файл
    let err = store
        .ls("Projects/uwuwu/Деплоймент.md/nosuch")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("файл"), "msg: {msg}");
    assert!(
        msg.contains("read \"Projects/uwuwu/Деплоймент\""),
        "msg: {msg}"
    );
    assert!(!msg.contains("Деплоймент.md/nosuch\""), "msg: {msg}");
    Ok(())
}

#[tokio::test]
async fn ls_known_paths_list_children_unchanged() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-happy").await?;
    let names = |e: &[DocEntry]| -> Vec<String> { e.iter().map(|x| x.name.clone()).collect() };
    assert_eq!(
        names(&store.ls("").await?),
        ["Daily/", "Experience/", "Projects/"]
    );
    assert_eq!(names(&store.ls("Projects").await?), ["origa/", "uwuwu/"]);
    assert_eq!(
        names(&store.ls("Projects/uwuwu").await?),
        ["ADR/", "Деплоймент.md"]
    );
    Ok(())
}

#[tokio::test]
async fn ls_parent_walker_is_rejected_as_unsafe() -> Result<()> {
    let tmp = TempDir::new()?;
    let store = ls_store(tmp.path(), "ls-walker").await?;
    let err = store.ls("Projects/../../etc").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("небезопасен"), "msg: {msg}");
    Ok(())
}

#[tokio::test]
async fn children_sorted_on_empty_folder_yields_no_entries() -> Result<()> {
    let tmp = TempDir::new()?;
    let empty = tmp.path().join("docs/empty");
    std::fs::create_dir_all(&empty)?;
    let entries = crate::store::github_doc_ls::children_sorted(&empty)?;
    assert!(entries.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_hint_on_empty_ancestor_names_it_empty() -> Result<()> {
    // пустая папка-предок возможна в dirty-клоне (mkdir без коммита) —
    // раскладка без висячего «доступно в «X»: » (ревью Low, задача 1094)
    let tmp = TempDir::new()?;
    std::fs::create_dir_all(tmp.path().join("docs/Ранбуки"))?;
    let store = GitHubDocStore::new("local/test".into(), tmp.path().to_path_buf());
    let err = store.missing_hint("Ранбуки/nosuch", "Ранбуки/nosuch");
    let msg = format!("{err:#}");
    assert!(msg.contains("«Ранбуки» пуст"), "msg: {msg}");
    assert!(!msg.contains("доступно"), "msg: {msg}");
    Ok(())
}
