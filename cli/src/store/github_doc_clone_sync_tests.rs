//! Юнит: sync-семантика общего клон-каталога (задача 1062) — dirty/ahead/
//! behind/параллелизм на живом риге формы cli: bare origin + writer-клон +
//! work-клон (init + remote + fetch --depth 1 + checkout), то есть shallow.

use crate::store::doc_store::{DocRef, DocStore};
use crate::store::github_doc_store::GitHubDocStore;
use crate::store::git_util::{git_raw, git_sync_with_author};
use anyhow::Result;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

async fn setup_rig(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let origin = dir.join(format!("{name}-origin"));
    let writer = dir.join(format!("{name}-writer"));
    let work = dir.join(format!("{name}-work"));
    for path in [&origin, &writer, &work] {
        std::fs::create_dir_all(path)?;
    }
    let origin_s = origin.to_str().unwrap();
    git_raw(&["init", "--bare", "-q", "-b", "main", origin_s]).await?;

    let w = writer.to_str().unwrap();
    git_raw(&["-C", w, "init", "-q", "-b", "main"]).await?;
    let person = writer.join("docs/Experience/person/PERSON.md");
    std::fs::create_dir_all(person.parent().unwrap())?;
    std::fs::write(&person, "база\n")?;
    git_raw(&["-C", w, "add", "-A"]).await?;
    git_sync_with_author(&["-C", w, "commit", "-q", "-m", "X1"])?;
    git_raw(&["-C", w, "push", "-q", origin_s, "main"]).await?;

    let k = work.to_str().unwrap();
    git_raw(&["-C", k, "init", "-q", "-b", "main"]).await?;
    git_raw(&["-C", k, "remote", "add", "origin", origin_s]).await?;
    git_raw(&["-C", k, "fetch", "-q", "--depth", "1", "origin"]).await?;
    git_raw(&["-C", k, "checkout", "-qf", "-B", "main", "origin/main"]).await?;
    Ok((origin, writer, work))
}

/// Пуш следующего коммита из writer (append к PERSON.md).
async fn writer_push(origin: &Path, writer: &Path, tag: &str) -> Result<()> {
    let w = writer.to_str().unwrap();
    let person = writer.join("docs/Experience/person/PERSON.md");
    let mut body = std::fs::read_to_string(&person)?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(tag);
    body.push('\n');
    std::fs::write(&person, body)?;
    git_raw(&["-C", w, "add", "-A"]).await?;
    git_sync_with_author(&["-C", w, "commit", "-q", "-m", tag])?;
    git_raw(&["-C", w, "push", "-q", origin.to_str().unwrap(), "main"]).await?;
    Ok(())
}

/// Локальный (незапушенный) коммит в work-клоне.
async fn work_commit(work: &Path, file: &str, content: &str, tag: &str) -> Result<()> {
    let k = work.to_str().unwrap();
    let path = work.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    git_raw(&["-C", k, "add", "-A"]).await?;
    git_sync_with_author(&["-C", k, "commit", "-q", "-m", tag])?;
    Ok(())
}

fn test_store(work: &Path) -> GitHubDocStore {
    GitHubDocStore::new("local/test".into(), work.to_path_buf())
}

/// Грязный клон (чужая незакоммиченная правка) — read живёт на stale,
/// чужие файлы не сносятся. Регрессия: был громкий bail read-пути.
#[tokio::test]
async fn read_on_dirty_clone_reads_stale_and_preserves_files() -> Result<()> {
    let tmp = TempDir::new()?;
    let (_origin, _writer, work) = setup_rig(tmp.path(), "dirty").await?;
    std::fs::write(work.join("docs/draft.md"), "черновик\n")?;
    let store = test_store(&work);
    let body = store
        .read(&DocRef {
            slug: "Experience/person/PERSON".into(),
        })
        .await?;
    assert_eq!(body, "база\n");
    assert!(
        work.join("docs/draft.md").exists(),
        "чужой незакоммиченный файл снесён"
    );
    Ok(())
}

/// Незапушенный коммит переживает read-sync: origin не двигался.
#[tokio::test]
async fn ahead_commit_survives_read_sync() -> Result<()> {
    let tmp = TempDir::new()?;
    let (_origin, _writer, work) = setup_rig(tmp.path(), "ahead").await?;
    work_commit(
        &work,
        "docs/Experience/ahead/A.md",
        "локальная правка\n",
        "X1b",
    )
    .await?;
    let store = test_store(&work);
    let body = store
        .read(&DocRef {
            slug: "Experience/person/PERSON".into(),
        })
        .await?;
    assert_eq!(body, "база\n");
    assert!(
        work.join("docs/Experience/ahead/A.md").exists(),
        "незапушенный коммит снесён sync'ом"
    );
    Ok(())
}

/// Незапушенный коммит переживает read-sync при дивергенции
/// (origin ушёл вперёд, локальный коммит поверх старого tip).
#[tokio::test]
async fn ahead_commit_survives_read_sync_diverged() -> Result<()> {
    let tmp = TempDir::new()?;
    let (origin, writer, work) = setup_rig(tmp.path(), "diverged").await?;
    writer_push(&origin, &writer, "X2").await?;
    work_commit(
        &work,
        "docs/Experience/ahead/A.md",
        "локальная правка\n",
        "X1b",
    )
    .await?;
    let store = test_store(&work);
    store
        .read(&DocRef {
            slug: "Experience/person/PERSON".into(),
        })
        .await?;
    assert!(
        work.join("docs/Experience/ahead/A.md").exists(),
        "незапушенный коммит снесён sync'ом при дивергенции"
    );
    Ok(())
}

/// Behind-синк (главный путь): origin ушёл вперёд, дерево чистое —
/// worktree догоняет origin/main.
#[tokio::test]
async fn behind_sync_fast_forwards_worktree() -> Result<()> {
    let tmp = TempDir::new()?;
    let (origin, writer, work) = setup_rig(tmp.path(), "behind").await?;
    writer_push(&origin, &writer, "X2").await?;
    let store = test_store(&work);
    let body = store
        .read(&DocRef {
            slug: "Experience/person/PERSON".into(),
        })
        .await?;
    assert_eq!(body, "база\nX2\n", "behind-синк не обновил клон");
    Ok(())
}

/// Write при грязном клоне — громкий отказ, чужие файлы целы.
#[tokio::test]
async fn write_on_dirty_clone_fails_and_preserves_foreign_files() -> Result<()> {
    let tmp = TempDir::new()?;
    let (_origin, _writer, work) = setup_rig(tmp.path(), "wdirty").await?;
    std::fs::write(work.join("docs/draft.md"), "черновик\n")?;
    let store = test_store(&work);
    let target = work.join("docs/Experience/person/PERSON.md");
    let result = store
        .write_files(&[(target, "новое тело\n".as_bytes().to_vec())], "wiki: x")
        .await;
    assert!(
        result.is_err(),
        "write поверх чужой грязи должен отказывать"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("docs/draft.md"))?,
        "черновик\n",
        "чужая грязь снесена write-путём"
    );
    Ok(())
}

/// Write при незапушенном коммите — громкий отказ, коммит цел.
#[tokio::test]
async fn write_on_ahead_clone_fails_and_preserves_commit() -> Result<()> {
    let tmp = TempDir::new()?;
    let (_origin, _writer, work) = setup_rig(tmp.path(), "wahead").await?;
    work_commit(
        &work,
        "docs/Experience/ahead/A.md",
        "локальная правка\n",
        "X1b",
    )
    .await?;
    let store = test_store(&work);
    let target = work.join("docs/Experience/person/PERSON.md");
    let result = store
        .write_files(&[(target, "новое тело\n".as_bytes().to_vec())], "wiki: x")
        .await;
    assert!(
        result.is_err(),
        "write поверх незапушенного коммита должен отказывать"
    );
    assert!(
        work.join("docs/Experience/ahead/A.md").exists(),
        "незапушенный коммит снесён write-путём"
    );
    Ok(())
}

/// Параллельная нагрузка: N одновременных read-sync на одном клоне и
/// first_checkout-гонка двух процессов на полу-клоне — все succeed.
#[tokio::test]
async fn parallel_syncs_and_first_checkouts_all_succeed() -> Result<()> {
    let tmp = TempDir::new()?;
    let (origin, _writer, work) = setup_rig(tmp.path(), "par").await?;

    let store = std::sync::Arc::new(test_store(&work));
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .read(&DocRef {
                    slug: "Experience/person/PERSON".into(),
                })
                .await
        }));
    }
    for task in tasks {
        let body = task.await.expect("join")?;
        assert_eq!(body, "база\n");
    }

    // first_checkout-гонка: полу-клон без origin/main ref, два параллельных
    // ensure_clone — сериализуются локом, второй проходит через sync-ветку.
    let fresh = tmp.path().join("fresh-work");
    std::fs::create_dir_all(&fresh)?;
    let fresh_s = fresh.to_str().unwrap();
    git_raw(&["-C", fresh_s, "init", "-q", "-b", "main"]).await?;
    git_raw(&[
        "-C",
        fresh_s,
        "remote",
        "add",
        "origin",
        origin.to_str().unwrap(),
    ])
    .await?;
    let stores: Vec<_> = (0..2)
        .map(|_| std::sync::Arc::new(test_store(&fresh)))
        .collect();
    let mut fresh_tasks = Vec::new();
    for store in stores {
        fresh_tasks.push(tokio::spawn(async move {
            store
                .read(&DocRef {
                    slug: "Experience/person/PERSON".into(),
                })
                .await
        }));
    }
    for task in fresh_tasks {
        let body = task.await.expect("join")?;
        assert_eq!(body, "база\n");
    }
    Ok(())
}
