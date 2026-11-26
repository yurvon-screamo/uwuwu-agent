//! S3-носители blob-канала (#1292): прод — Tigris (rusty-s3 SigV4,
//! query-presign; reqwest — транспорт), e2e/пробы — память. Контракты
//! уровня носителя; тул-поверхность тестируется в api/tools_tests.

use std::time::Duration;

use async_trait::async_trait;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use uwuwu_domain::BlobKey;
use uwuwu_usecase::{
    BlobStore, UsecaseError, UsecaseResult, BLOB_PRESIGN_PUT_TTL_SECS, BLOB_PRESIGN_TTL_SECS,
};

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

    fn presign_put_url(&self, key: &BlobKey, content_type: &str) -> String {
        let mut action = self
            .bucket
            .put_object(Some(&self.credentials), key.as_str());
        action.headers_mut().insert("content-type", content_type);
        action
            .sign(Duration::from_secs(BLOB_PRESIGN_PUT_TTL_SECS))
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

    fn presign_put_url(&self, key: &BlobKey, _content_type: &str) -> String {
        format!("memory-put://{}", key.as_str())
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

    #[test]
    fn presign_put_url_carries_short_ttl_and_signed_content_type() {
        let store = fake_store();
        let key = BlobKey::parse("blobs/uwuwu/1292/a1b2c3d4-скрин.png").expect("ключ валиден");
        let url = store.presign_put_url(&key, "image/png");
        assert!(url.contains("X-Amz-Expires=300"), "{url}");
        assert!(
            url.contains("X-Amz-SignedHeaders=content-type%3Bhost")
                || url.contains("X-Amz-SignedHeaders=content-type;host"),
            "тип в подписи: {url}"
        );
        assert!(url.contains("X-Amz-Signature="), "{url}");
        assert!(!url.contains(' '), "кириллица percent-encoded: {url}");
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

    /// Интеграционная граница (тестировщик #1292): TigrisBlobStore через
    /// реальный HTTP-транспорт (reqwest) против локального фейка S3.
    /// VirtualHost-стиль ведёт на `<bucket>.localhost` — RFC 6761 резолвит
    /// `*.localhost` в loopback; резолвера нет — стенд пропускается.
    #[tokio::test]
    async fn tigris_store_roundtrip_over_real_http_transport() {
        use axum::extract::{Path, State};
        use axum::response::Response;
        use std::net::ToSocketAddrs;
        use std::sync::Arc;
        if "uwuwu-blobs-it.localhost:1".to_socket_addrs().is_err() {
            eprintln!("*.localhost не резолвится — интеграционный стенд пропущен");
            return;
        }

        type Storage = Arc<Mutex<Vec<(String, Vec<u8>, String)>>>;
        let stored: Storage = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let app = {
            let stored = stored.clone();
            axum::Router::new()
                .route(
                    "/{*key}",
                    axum::routing::any(
                        move |method: axum::http::Method,
                              uri: axum::http::Uri,
                              headers: axum::http::HeaderMap,
                              State(stored): State<Storage>,
                              Path(key): Path<String>,
                              body: axum::body::Bytes| {
                            async move {
                                let respond =
                                    |status: axum::http::StatusCode,
                                     ctype: Option<&str>,
                                     body: Vec<u8>| {
                                        let mut builder = Response::builder().status(status);
                                        if let Some(ctype) = ctype {
                                            builder = builder.header("content-type", ctype);
                                        }
                                        builder
                                            .body(axum::body::Body::from(body))
                                            .expect("static response")
                                    };
                                match method {
                                    axum::http::Method::PUT => {
                                        let ctype = headers
                                            .get("content-type")
                                            .and_then(|v| v.to_str().ok())
                                            .unwrap_or_default()
                                            .to_string();
                                        stored.lock().expect("lock").push((
                                            key,
                                            body.to_vec(),
                                            ctype,
                                        ));
                                        respond(axum::http::StatusCode::OK, None, Vec::new())
                                    }
                                    m if m == axum::http::Method::GET
                                        || m == axum::http::Method::HEAD =>
                                    {
                                        let signed = uri
                                            .query()
                                            .map(|q| q.contains("X-Amz-Signature="))
                                            .unwrap_or(false);
                                        let hit = signed.then(|| {
                                            stored
                                                .lock()
                                                .expect("lock")
                                                .iter()
                                                .find(|(k, _, _)| *k == key)
                                                .map(|(_, b, c)| (b.clone(), c.clone()))
                                        });
                                        match hit.flatten() {
                                            Some((bytes, ctype)) => respond(
                                                axum::http::StatusCode::OK,
                                                Some(ctype.as_str()),
                                                bytes,
                                            ),
                                            None if signed => respond(
                                                axum::http::StatusCode::NOT_FOUND,
                                                None,
                                                Vec::new(),
                                            ),
                                            None => respond(
                                                axum::http::StatusCode::FORBIDDEN,
                                                Some("application/xml"),
                                                b"<Error><Code>AccessDenied</Code></Error>"
                                                    .to_vec(),
                                            ),
                                        }
                                    }
                                    _ => respond(
                                        axum::http::StatusCode::METHOD_NOT_ALLOWED,
                                        None,
                                        Vec::new(),
                                    ),
                                }
                            }
                        },
                    ),
                )
                .with_state(stored.clone())
        };
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let store = TigrisBlobStore::new(TigrisConfig {
            endpoint: format!("http://localhost:{port}"),
            region: "auto".to_string(),
            bucket: "uwuwu-blobs-it".to_string(),
            access_key: "tid_it".to_string(),
            secret: "tid_it".to_string(),
        })
        .expect("конфиг валиден");
        let key = BlobKey::parse("blobs/uwuwu/1292/a1b2c3d4-скрин.png").expect("ключ валиден");

        store
            .put(&key, "image/png", b"\x89PNG-it")
            .await
            .expect("put через реальный HTTP-транспорт");
        {
            let guard = stored.lock().expect("lock");
            let (_, bytes, ctype) = guard.first().expect("фейк принял объект");
            assert_eq!(bytes.as_slice(), b"\x89PNG-it", "байты дошли до носителя");
            assert_eq!(ctype, "image/png", "content-type дошёл до носителя");
        }

        let url = store.presign_get_url(&key);
        let fetched = reqwest::get(&url)
            .await
            .expect("GET по presigned-ссылке")
            .error_for_status()
            .expect("presigned GET успешен");
        assert_eq!(
            fetched.bytes().await.expect("body").as_ref(),
            b"\x89PNG-it",
            "presigned GET возвращает записанные байты"
        );

        let anon = url.split('?').next().expect("path").to_string();
        let status = reqwest::get(&anon).await.expect("anon GET").status();
        assert_eq!(
            status,
            reqwest::StatusCode::FORBIDDEN,
            "GET без подписи — 403 (приватный бакет)"
        );

        let missing = BlobKey::parse("blobs/uwuwu/misc/ffffffff-none.png").expect("ключ валиден");
        let missing_url = store.presign_get_url(&missing);
        let resp = reqwest::get(&missing_url).await.expect("GET missing");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "L-1: presign локален — URL на несуществующий ключ успешен, 404 на открытии"
        );

        let direct_key =
            BlobKey::parse("blobs/uwuwu/misc/f0f0f0f0-direct.png").expect("ключ валиден");
        let direct_url = store.presign_put_url(&direct_key, "image/png");
        reqwest::Client::new()
            .put(direct_url)
            .header("content-type", "image/png")
            .body("agent-bytes")
            .send()
            .await
            .expect("agent PUT by presigned URL")
            .error_for_status()
            .expect("PUT через presign успешен");
        assert!(
            stored.lock().expect("lock").iter().any(|(k, b, c)| {
                k == direct_key.as_str() && b.as_slice() == b"agent-bytes" && c == "image/png"
            }),
            "байты агента через presign дошли до носителя"
        );
    }
}
