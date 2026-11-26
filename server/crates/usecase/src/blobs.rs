//! Blob-канал (#1292): файлы агентов → приватный S3-бакет (Tigris),
//! ссылки — presigned GET с потолком SigV4 (7 суток). Носитель — порт
//! (`BlobStore`); юзкейс держит инварианты лимита, санитайза имени и
//! дедуп-ключа; домен — структуру ключа (`BlobKey`).

use crate::bots::normalize_asset_name;
use crate::bots::MediaKind;
use crate::error::{UsecaseError, UsecaseResult};
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use uwuwu_domain::BlobKey;

/// Потолок файла: прецедент UC-06 (Bot API getFile ≤ 20 MiB).
pub const BLOB_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Потолок SigV4: presigned URL живёт максимум 7 суток (604 800 с) —
/// больше SigV4 не подписывает.
pub const BLOB_PRESIGN_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Срок presigned PUT для заливки агентом: байты идут из shell мимо
/// контекста модели; подпись живёт несколько минут, не сутки.
pub const BLOB_PRESIGN_PUT_TTL_SECS: u64 = 5 * 60;

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
    /// Presigned PUT ровно на этот ключ: агент заливает байты напрямую,
    /// сервер их не видит. Content-Type входит в подпись.
    fn presign_put_url(&self, key: &BlobKey, content_type: &str) -> String;
}

/// Квитанция blob_put: ключ + ссылка + фактический content-type/размер.
#[derive(Debug)]
pub struct BlobPutReceipt {
    pub key: String,
    pub url: String,
    pub content_type: String,
    pub size: usize,
}

/// Квитанция резервирования: ключ и обе подписи, байты ещё не залиты.
#[derive(Debug)]
pub struct BlobReserveReceipt {
    pub key: String,
    pub put_url: String,
    pub url: String,
    pub content_type: String,
    pub put_ttl_secs: u64,
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
        let (key, name) = build_key(target, &sha8)?;
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

    /// Резервирование ключа под прямую заливку: байты не приходят в тул,
    /// поэтому дедуп по sha8 невозможен — каждый вызов даёт свежий UUID.
    pub fn reserve(&self, target: &BlobPlace) -> UsecaseResult<BlobReserveReceipt> {
        let store = self.store()?;
        let upload_id = Uuid::new_v4().to_string();
        let (key, name) = build_key(target, &upload_id)?;
        let content_type = content_type_for(&name);
        Ok(BlobReserveReceipt {
            put_url: store.presign_put_url(&key, &content_type),
            url: store.presign_get_url(&key),
            key: key.as_str().to_string(),
            content_type,
            put_ttl_secs: BLOB_PRESIGN_PUT_TTL_SECS,
        })
    }

    /// Переподпись истёкшей ссылки по ключу. Носитель не правит — замену
    /// URL в теле/комменте/доке делает агент текстовыми тулами.
    pub fn fresh_url(&self, key: &BlobKey) -> UsecaseResult<String> {
        Ok(self.store()?.presign_get_url(key))
    }
}

/// Единый канон ключа для обоих режимов: `<project>/<task|misc>/<id>-<name>`.
/// Реюз санитайза #1171: сепараторы/control → '_', вырожденное имя →
/// фолбэк `file-<id>.bin`. `<id>` — sha8 байтов или UUID загрузки.
fn build_key(target: &BlobPlace, id: &str) -> UsecaseResult<(BlobKey, String)> {
    let name = normalize_asset_name(Some(&target.filename), MediaKind::Document, id);
    let task_segment = target
        .task
        .map_or_else(|| "misc".to_string(), |n| n.to_string());
    let raw_key = format!("blobs/{}/{task_segment}/{id}-{name}", target.project.trim());
    let key = BlobKey::parse(&raw_key).ok_or_else(|| {
        UsecaseError::BlobBadKey(format!("the blob key failed the guard: {raw_key}"))
    })?;
    Ok((key, name))
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
