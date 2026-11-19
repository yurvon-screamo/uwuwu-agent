//! S3-носители blob-канала (#1292): прод — Tigris (rusty-s3 SigV4,
//! query-presign; reqwest — транспорт), e2e/пробы — память. Контракты
//! уровня носителя; тул-поверхность тестируется в api/tools_tests.

use std::time::Duration;

use async_trait::async_trait;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use uwuwu_domain::BlobKey;
use uwuwu_usecase::{BlobStore, UsecaseError, UsecaseResult, BLOB_PRESIGN_TTL_SECS};

#[cfg(test)]
use std::sync::Mutex;

/// Срок presigned PUT: подпись тратится сразу при отправке — короткий
/// срок безопаснее (в отличие от GET-ссылки, живущей в носителях).
const PUT_SIGN_TTL_SECS: u64 = 60;

/// Имена env blob-канала: единый перечень для сборки в main и перечня
/// недостающих в громком отказе (`BlobNotConfigured`).
pub const S3_ENV_NAMES: [&str; 5] = [
    "UWUWU_S3_ENDPOINT",
    "UWUWU_S3_REGION",
    "UWUWU_S3_BUCKET",
    "UWUWU_S3_ACCESS_KEY",
    "UWUWU_S3_SECRET",
];

/// Значения env blob-канала: именованная сборка вместо позиционных
/// аргументов (ревью #1292 — перестановка позиций молча меняет семантику).
pub struct TigrisConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret: String,
}

/// Прод-носитель: приватный бакет Tigris (ACL private). Анонимные
/// GET/HEAD/List = 403; доступ только по presigned-подписи.
#[derive(Debug)]
pub struct TigrisBlobStore {
    bucket: Bucket,
    credentials: Credentials,
    client: reqwest::Client,
}

impl TigrisBlobStore {
    /// Сборка из env-значений. Кривой endpoint/bucket — громкая ошибка:
    /// вызывающий распространяет её через Result бута (конвенция main),
    /// сервер не поднимается, но не паникует.
    pub fn new(config: TigrisConfig) -> Result<Self, String> {
        let endpoint = config
            .endpoint
            .parse()
            .map_err(|e| format!("UWUWU_S3_ENDPOINT: {e}"))?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::VirtualHost,
            config.bucket,
            config.region,
        )
        .map_err(|e| format!("UWUWU_S3_BUCKET: {e}"))?;
        Ok(Self {
            bucket,
            credentials: Credentials::new(config.access_key, config.secret),
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl BlobStore for TigrisBlobStore {
    async fn put(&self, key: &BlobKey, content_type: &str, bytes: &[u8]) -> UsecaseResult<()> {
        let mut action = self
            .bucket
            .put_object(Some(&self.credentials), key.as_str());
        action.headers_mut().insert("content-type", content_type);
        let url = action.sign(Duration::from_secs(PUT_SIGN_TTL_SECS));
        let response = self
            .client
            .put(url)
            .header("content-type", content_type)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|e| UsecaseError::Blob(format!("put {}: {e}", key.as_str())))?;
        response
            .error_for_status()
            .map_err(|e| UsecaseError::Blob(format!("put {}: {e}", key.as_str())))?;
        Ok(())
    }

    fn presign_get_url(&self, key: &BlobKey) -> String {
        self.bucket
            .get_object(Some(&self.credentials), key.as_str())
            .sign(Duration::from_secs(BLOB_PRESIGN_TTL_SECS))
            .to_string()
    }
}

/// Стенд (e2e/пробы): фиксирует key ↔ байты в памяти — полный туловой
/// контур без GitHub/S3 (образец — MemoryAssetStore #1171). Живёт только
/// в тестах: прод-бинарь мёртвый код не тащит.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryBlobStore {
    files: Mutex<Vec<(String, Vec<u8>, String)>>,
}

#[cfg(test)]
impl MemoryBlobStore {
    /// Снимок содержимого для ассертов тестов/стенда.
    pub fn contents(&self) -> Vec<(String, Vec<u8>, String)> {
        self.files.lock().expect("blobs lock").clone()
    }
}

#[cfg(test)]
#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put(&self, key: &BlobKey, content_type: &str, bytes: &[u8]) -> UsecaseResult<()> {
        self.files.lock().expect("blobs lock").push((
            key.as_str().to_string(),
            bytes.to_vec(),
            content_type.to_string(),
        ));
        Ok(())
    }

    fn presign_get_url(&self, key: &BlobKey) -> String {
        format!("memory://{}", key.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_store() -> TigrisBlobStore {
        TigrisBlobStore::new(TigrisConfig {
            endpoint: "https://t3.storageapi.dev".to_string(),
            region: "auto".to_string(),
            bucket: "uwuwu-blobs".to_string(),
            access_key: "tid_key".to_string(),
            secret: "tid_secret".to_string(),
        })
        .expect("конфиг валиден")
    }

    #[test]
    fn new_rejects_invalid_endpoint_loudly() {
        let err = TigrisBlobStore::new(TigrisConfig {
            endpoint: "not-a-url".to_string(),
            region: "auto".to_string(),
            bucket: "b".to_string(),
            access_key: "k".to_string(),
            secret: "s".to_string(),
        })
        .expect_err("громкий отказ конфига");
        assert!(err.contains("UWUWU_S3_ENDPOINT"), "{err}");
    }

    #[test]
    fn presign_get_url_carries_sigv4_query_and_bucket_host() {
        let store = fake_store();
        let key = BlobKey::parse("blobs/uwuwu/1292/a1b2c3d4-скрин.png").expect("ключ валиден");
        let url = store.presign_get_url(&key);
        assert!(
            url.starts_with("https://uwuwu-blobs.t3.storageapi.dev/"),
            "{url}"
        );
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"), "{url}");
        assert!(url.contains("X-Amz-Expires=604800"), "потолок SigV4: {url}");
        assert!(url.contains("X-Amz-SignedHeaders=host"), "{url}");
        assert!(url.contains("X-Amz-Signature="), "{url}");
        // Кириллица/пробелы — percent-encoded, путь не рвётся
        assert!(!url.contains(' '), "{url}");
    }

    #[tokio::test]
    async fn memory_store_roundtrips_key_bytes_and_content_type() {
        let store = MemoryBlobStore::default();
        let key = BlobKey::parse("blobs/uwuwu/misc/a1b2c3d4-x.png").expect("ключ валиден");
        store
            .put(&key, "image/png", b"\x89PNG")
            .await
            .expect("put ok");
        assert_eq!(
            store.presign_get_url(&key),
            "memory://blobs/uwuwu/misc/a1b2c3d4-x.png"
        );
        let contents = store.contents();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].0, "blobs/uwuwu/misc/a1b2c3d4-x.png");
        assert_eq!(contents[0].1, b"\x89PNG");
        assert_eq!(contents[0].2, "image/png");
    }
}
