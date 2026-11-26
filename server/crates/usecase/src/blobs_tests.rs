//! Юнит-матрица blob-канала (#1292): лимит, дедуп-ключ, content-type,
//! идемпотентность, громкость отказа без конфига.

use super::{
    content_type_for, sha8_hex, BlobPlace, BlobUseCase, BLOB_MAX_BYTES, BLOB_PRESIGN_PUT_TTL_SECS,
};
use crate::error::UsecaseError;
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_domain::BlobKey;

struct RecordingStore;

#[async_trait]
impl super::BlobStore for RecordingStore {
    async fn put(
        &self,
        _key: &BlobKey,
        _content_type: &str,
        _bytes: &[u8],
    ) -> crate::error::UsecaseResult<()> {
        Ok(())
    }
    fn presign_get_url(&self, key: &BlobKey) -> String {
        format!("presigned://{}", key.as_str())
    }
    fn presign_put_url(&self, key: &BlobKey, _content_type: &str) -> String {
        format!("presigned-put://{}", key.as_str())
    }
}

fn place(filename: &str) -> BlobPlace {
    BlobPlace {
        project: "uwuwu".to_string(),
        task: Some(1292),
        filename: filename.to_string(),
    }
}

fn ready_case() -> BlobUseCase {
    BlobUseCase::ready(Arc::new(RecordingStore))
}

#[tokio::test]
async fn place_builds_dedup_key_and_returns_presigned_url() {
    let case = ready_case();
    let bytes = b"\x89PNG-bytes";
    let receipt = case
        .place(&place("скрин от МПНЧ.png"), bytes)
        .await
        .expect("place ok");
    assert_eq!(receipt.content_type, "image/png");
    assert_eq!(receipt.size, bytes.len());
    let key = receipt.key.clone();
    let tail = key
        .strip_prefix("blobs/uwuwu/1292/")
        .unwrap_or_default()
        .to_string();
    assert!(!tail.is_empty(), "ключ в сегменте задачи: {key}");
    assert!(tail.starts_with(&sha8_hex(bytes)), "дедуп-сегмент: {key}");
    assert!(tail.ends_with("скрин от МПНЧ.png"), "имя сохранено: {key}");
    assert_eq!(receipt.url, format!("presigned://{key}"));
}

#[test]
fn reserve_builds_fresh_key_with_both_presigned_urls() {
    let case = ready_case();
    let first = case.reserve(&place("скрин от МПНЧ.png")).expect("first");
    let second = case.reserve(&place("скрин от МПНЧ.png")).expect("second");

    assert_eq!(first.content_type, "image/png");
    assert_eq!(first.put_ttl_secs, BLOB_PRESIGN_PUT_TTL_SECS);
    let tail = first
        .key
        .strip_prefix("blobs/uwuwu/1292/")
        .unwrap_or_default();
    assert!(tail.ends_with("-скрин от МПНЧ.png"), "{tail}");
    assert!(
        tail.split('-').next().is_some_and(|id| id.len() == 8),
        "UUID-сегмент: {tail}"
    );
    assert_eq!(first.put_url, format!("presigned-put://{}", first.key));
    assert_eq!(first.url, format!("presigned://{}", first.key));
    assert_ne!(first.key, second.key, "каждая загрузка — новый ключ");
}

#[test]
fn reserve_sanitizes_filename_and_rejects_missing_config() {
    let case = ready_case();
    let reserved = case.reserve(&place("../../etc/passwd")).expect("санитайз");
    let segments: Vec<&str> = reserved.key.split('/').collect();
    assert_eq!(segments.len(), 4, "{}", reserved.key);
    assert!(
        segments
            .iter()
            .all(|s| !s.is_empty() && *s != "." && *s != ".."),
        "сегменты безопасны: {}",
        reserved.key
    );

    let missing = BlobUseCase::missing(vec!["UWUWU_S3_SECRET".to_string()]);
    let err = missing
        .reserve(&place("a.png"))
        .expect_err("громкий отказ без конфига");
    assert!(matches!(
        err,
        crate::error::UsecaseError::BlobNotConfigured(_)
    ));
}

#[tokio::test]
async fn same_file_placed_twice_lands_on_same_key() {
    // Идемпотентность: тот же контент + то же имя → тот же ключ
    // (sha8 + имя); имя в ключе — дедуп в рамках каталога.
    let case = ready_case();
    let first = case
        .place(&place("a.png"), b"identical")
        .await
        .expect("first");
    let second = case
        .place(&place("a.png"), b"identical")
        .await
        .expect("second");
    assert_eq!(first.key, second.key);
    let other = case
        .place(&place("a.png"), b"changed")
        .await
        .expect("other bytes");
    assert_ne!(first.key, other.key, "другой контент — другой ключ");
}

#[tokio::test]
async fn over_limit_file_is_a_loud_rejection() {
    let case = ready_case();
    let err = case
        .place(&place("big.bin"), &[0u8; BLOB_MAX_BYTES + 1])
        .await
        .expect_err("громкий отказ");
    assert!(matches!(err, UsecaseError::BlobTooBig(_)), "{err}");
}

#[tokio::test]
async fn unconfigured_channel_names_missing_env_vars() {
    let case = BlobUseCase::missing(vec![
        "UWUWU_S3_ENDPOINT".to_string(),
        "UWUWU_S3_SECRET".to_string(),
    ]);
    let err = case
        .place(&place("a.png"), b"x")
        .await
        .expect_err("громкий отказ");
    match err {
        UsecaseError::BlobNotConfigured(msg) => {
            assert!(msg.contains("UWUWU_S3_ENDPOINT"), "{msg}");
            assert!(msg.contains("UWUWU_S3_SECRET"), "{msg}");
        }
        other => panic!("не тот класс: {other}"),
    }
}

#[tokio::test]
async fn unsanitized_filename_stays_single_component() {
    let case = ready_case();
    let receipt = case
        .place(&place("../../etc/passwd"), b"x")
        .await
        .expect("санитайз нейтрализует сепараторы");
    // Контракт BlobKey: ни один СЕГМЕНТ ключа не равен "."/".."
    // (подстрока ".." внутри нейтрализованного имени — не путь).
    let segments: Vec<&str> = receipt.key.split('/').collect();
    assert_eq!(segments.len(), 4, "4 сегмента: {}", receipt.key);
    assert!(
        segments
            .iter()
            .all(|s| *s != "." && *s != ".." && !s.is_empty()),
        "сегменты безопасны: {}",
        receipt.key
    );
}

#[tokio::test]
async fn fresh_url_resigns_by_key() {
    let case = ready_case();
    let key = BlobKey::parse("blobs/uwuwu/misc/a1b2c3d4-x.png").expect("валидный ключ");
    assert_eq!(
        case.fresh_url(&key).expect("fresh ok"),
        "presigned://blobs/uwuwu/misc/a1b2c3d4-x.png"
    );
}

#[test]
fn content_type_covers_screens_and_docs() {
    assert_eq!(content_type_for("скрин.png"), "image/png");
    assert_eq!(content_type_for("фото.JPG"), "image/jpeg");
    assert_eq!(content_type_for("runbook.md"), "text/plain");
    assert_eq!(content_type_for("chart.svg"), "image/svg+xml");
    assert_eq!(content_type_for("no-extension"), "application/octet-stream");
}
