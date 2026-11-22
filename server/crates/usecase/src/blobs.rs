//! Blob-канал (#1292): файлы агентов → приватный S3-бакет (Tigris),
//! ссылки — presigned GET с потолком SigV4 (7 суток). Носитель — порт
//! (`BlobStore`); юзкейс держит инварианты лимита, санитайза имени и
//! дедуп-ключа; домен — структуру ключа (`BlobKey`).

use crate::bots::normalize_asset_name;
use crate::bots::MediaKind;
use crate::error::{UsecaseError, UsecaseResult};
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_domain::BlobKey;

/// Потолок файла: прецедент UC-06 (Bot API getFile ≤ 20 MiB).
pub const BLOB_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Потолок SigV4: presigned URL живёт максимум 7 суток (604 800 с) —
/// больше SigV4 не подписывает.
pub const BLOB_PRESIGN_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Цель вложения: проект обязателен (сегмент ключа), задача опциональна
/// (без неё сегмент `misc` — вложения к докам вики и пр.).
pub struct BlobPlace {
    pub project: String,
    pub task: Option<u64>,
    pub filename: String,
}

/// S3-носитель blob-канала: запись объекта + локальная presign-подпись.
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, key: &BlobKey, content_type: &str, bytes: &[u8]) -> UsecaseResult<()>;
    /// Presign — локальная подпись, сетевых вызовов нет: URL на
    /// несуществующий ключ вернётся успешно (упадёт 403 при открытии).
    fn presign_get_url(&self, key: &BlobKey) -> String;
}

/// Квитанция blob_put: ключ + ссылка + фактический content-type/размер.
#[derive(Debug)]
pub struct BlobPutReceipt {
    pub key: String,
    pub url: String,
    pub content_type: String,
    pub size: usize,
}

/// Живой носитель или перечень недостающих env — громкий отказ (ADR-010):
/// вызов тулов без конфига возвращает перечень, не тихую слепоту.
enum BlobBackend {
    Ready(Arc<dyn BlobStore>),
    Missing(Vec<String>),
}

pub struct BlobUseCase {
    backend: BlobBackend,
}

impl BlobUseCase {
    pub fn ready(store: Arc<dyn BlobStore>) -> Self {
        Self {
            backend: BlobBackend::Ready(store),
        }
    }

    pub fn missing(env_names: Vec<String>) -> Self {
        Self {
            backend: BlobBackend::Missing(env_names),
        }
    }

    fn store(&self) -> UsecaseResult<&Arc<dyn BlobStore>> {
        match &self.backend {
            BlobBackend::Ready(store) => Ok(store),
            BlobBackend::Missing(names) => Err(UsecaseError::BlobNotConfigured(format!(
                "set the variables: {}",
                names.join(", ")
            ))),
        }
    }

    /// place: лимит → санитайз имени → дедуп-ключ → put → presign.
    /// Повторный put тех же байтов даёт тот же ключ (идемпотентно).
    pub async fn place(&self, target: &BlobPlace, bytes: &[u8]) -> UsecaseResult<BlobPutReceipt> {
        let store = self.store()?;
        if bytes.len() > BLOB_MAX_BYTES {
            return Err(UsecaseError::BlobTooBig(format!(
                "{} bytes over the {} limit (the UC-06 precedent)",
                bytes.len(),
                BLOB_MAX_BYTES
            )));
        }
        let sha8 = sha8_hex(bytes);
        // Реюз санитайза #1171: сепараторы/control → '_', вырожденное
        // имя → фолбэк file-<sha8>.bin (sha8 уникализует).
        let name = normalize_asset_name(Some(&target.filename), MediaKind::Document, &sha8);
        let task_segment = target
            .task
            .map_or_else(|| "misc".to_string(), |n| n.to_string());
        let raw_key = format!(
            "blobs/{}/{task_segment}/{sha8}-{name}",
            target.project.trim()
        );
        let key = BlobKey::parse(&raw_key).ok_or_else(|| {
            UsecaseError::BlobBadKey(format!("the dedup key failed the guard: {raw_key}"))
        })?;
        let content_type = content_type_for(&name);
        store.put(&key, &content_type, bytes).await?;
        let url = store.presign_get_url(&key);
        Ok(BlobPutReceipt {
            key: key.as_str().to_string(),
            url,
            content_type,
            size: bytes.len(),
        })
    }

    /// Переподпись истёкшей ссылки по ключу. Носитель не правит — замену
    /// URL в теле/комменте/доке делает агент текстовыми тулами.
    pub fn fresh_url(&self, key: &BlobKey) -> UsecaseResult<String> {
        Ok(self.store()?.presign_get_url(key))
    }
}

/// Первые 8 hex SHA-256: дедуп-сегмент ключа (одинаковые байты → один ключ).
fn sha8_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Content-Type по расширению: скриновый/доковый минимум, остальное —
/// octet-stream (браузер отдаёт на скачивание, не ломает рендер).
fn content_type_for(filename: &str) -> String {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "txt" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod blobs_tests;
