//! Юнит write-path docs (create/append/replace/delete/tree — задача 46):
//! гварды входа, round-trip, --base свежесть. Контур — живой git
//! (риг github_doc_rig: bare origin + рабочий клон).

use crate::store::doc_store::{DocRef, DocStore};
use crate::store::github_doc_guards::sha256_hex;
use crate::store::github_doc_rig::{hub, rig, ADR_DOC, NEW_DOC, PLAIN_DOC};
use anyhow::Result;

#[tokio::test]
async fn create_writes_header_and_strips_title_h1() -> Result<()> {
    let (_tmp, work) = rig("create").await?;
    let out = hub(&work)
        .create_doc(NEW_DOC, "# new-doc\n\nтело входа\n")
        .await?;
    assert!(out.warnings.is_empty());
    assert!(out.doc.contains("title: new-doc"));
    // title-H1 стрип: дубль заголовка из входа не дублируется
    assert_eq!(out.doc.matches("# new-doc").count(), 1);
    assert!(out.doc.contains("тело входа"));
    Ok(())
}

#[tokio::test]
async fn create_rejects_existing_and_frontmatter_and_unsafe_slug() -> Result<()> {
    let (_tmp, work) = rig("guards").await?;
    let store = hub(&work);
    let err = store.create_doc(ADR_DOC, "тело").await.unwrap_err();
    assert!(err.to_string().contains("существует"));
    let err = store
        .create_doc(NEW_DOC, "---\ntitle: x\n---\n\nтело\n")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("frontmatter"));
    let err = store
        .create_doc("Projects/../escape", "тело")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("небезопасен"));
    let err = store
        .create_doc("Experience/a//b", "тело")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("небезопасен"));
    Ok(())
}

#[tokio::test]
async fn create_warns_on_taken_adr_number_and_new_folder() -> Result<()> {
    let (_tmp, work) = rig("adr").await?;
    let store = hub(&work);
    let out = store
        .create_doc("Projects/proj/ADR-001: дубликат-номера", "тело")
        .await?;
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("ADR-номер 001 занят")),
        "warnings: {:?}",
        out.warnings
    );
    let out = store
        .create_doc("Experience/новая-тема/первый-док", "тело")
        .await?;
    assert!(
        out.warnings.iter().any(|w| w.contains("новая-тема» новая")),
        "warnings: {:?}",
        out.warnings
    );
    Ok(())
}

#[tokio::test]
async fn create_append_replace_roundtrip() -> Result<()> {
    let (_tmp, work) = rig("rt").await?;
    let store = hub(&work);
    store.create_doc(NEW_DOC, "тело-1\n").await?;
    let after_append = store.append_doc(NEW_DOC, "строка-2").await?;
    assert!(after_append.contains("тело-1"));
    assert!(after_append.ends_with("строка-2\n"));
    let fresh = store
        .read(&DocRef {
            slug: NEW_DOC.into(),
        })
        .await?;
    let out = store
        .replace_doc(NEW_DOC, &fresh, Some(&sha256_hex(fresh.as_bytes())))
        .await?;
    assert_eq!(out, fresh);
    Ok(())
}

#[tokio::test]
async fn replace_base_rejects_stale_snapshot() -> Result<()> {
    let (_tmp, work) = rig("stale").await?;
    let store = hub(&work);
    let fresh = store
        .read(&DocRef {
            slug: PLAIN_DOC.into(),
        })
        .await?;
    // чужая правка между read и replace
    store.append_doc(PLAIN_DOC, "чужая дельта").await?;
    let err = store
        .replace_doc(
            PLAIN_DOC,
            "перезапись поверх чужой дельты",
            Some(&sha256_hex(fresh.as_bytes())),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("менялся с момента чтения"));
    Ok(())
}

#[tokio::test]
async fn append_requires_existing_doc() -> Result<()> {
    let (_tmp, work) = rig("append-strict").await?;
    let store = hub(&work);
    let missing = "Experience/theme/нет-такого";
    let err = store.append_doc(missing, "добавка").await.unwrap_err();
    assert!(err.to_string().contains("docs create"));
    assert!(!store.exists(missing).await?);
    Ok(())
}

#[tokio::test]
async fn delete_removes_doc_and_rejects_folder() -> Result<()> {
    let (_tmp, work) = rig("delete").await?;
    let store = hub(&work);
    store.delete_doc(PLAIN_DOC).await?;
    assert!(!store.exists(PLAIN_DOC).await?);
    let err = store
        .read(&DocRef {
            slug: PLAIN_DOC.into(),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("не найден"));
    let err = store.delete_doc("Projects/proj").await.unwrap_err();
    assert!(err.to_string().contains("папка"));
    Ok(())
}

#[tokio::test]
async fn tree_lists_sorted_slugs() -> Result<()> {
    let (_tmp, work) = rig("tree").await?;
    let slugs = hub(&work).tree().await?;
    assert_eq!(
        slugs,
        vec![
            "Experience/theme/plain-doc".to_string(),
            "Projects/proj/ADR-001: тест-решение".to_string(),
        ]
    );
    Ok(())
}
