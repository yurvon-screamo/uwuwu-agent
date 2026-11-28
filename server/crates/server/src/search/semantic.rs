//! Эмбеддинги: локальная Ollama embeddinggemma-2 через /api/embed (#1687,
//! слово владельца 26.11 — возврат с внешнего Gemini free tier).
//! Контракт модели (ollama.com/library/embeddinggemma-2): 768-dim,
//! task-steered префиксы doc/query совпадают с прежним клиентом, MRL-усечение
//! через options не применяется API. Отказ маршрута — типизированная ошибка.

use async_trait::async_trait;
use std::time::Duration;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

/// Модель с явным тегом: голого `embeddinggemma-2` в хранилище нет.
pub fn embed_model() -> String {
    uwuwu_usecase::runtime_cfg::search::model()
}
/// Нативная размерность модели — из `[search] embed_dims` config.toml.
pub fn search_dims() -> usize {
    uwuwu_usecase::runtime_cfg::search::dims()
}
/// Текстов в одном /api/embed-вызове — из `[search] embed_batch`.
pub fn embed_batch() -> usize {
    uwuwu_usecase::runtime_cfg::search::batch()
}
const SERVICE: &str = "ollama-embeddings";

/// Таймауты клиента: connect короткий, total — по классу вызова. Батч 60 с
/// ограничивает один /api/embed-вызов, гарда времени у тика нет (#1730);
/// query 15 с не держит поиск в стейл-туннеле.
#[derive(Debug, Clone, Copy)]
pub struct ClientTimeouts {
    pub connect: Duration,
    pub query_total: Duration,
    pub batch_total: Duration,
}

impl Default for ClientTimeouts {
    fn default() -> Self {
        use uwuwu_usecase::runtime_cfg::search;
        Self {
            connect: Duration::from_secs(search::connect_secs()),
            query_total: Duration::from_secs(search::query_total_secs()),
            batch_total: Duration::from_secs(search::batch_total_secs()),
        }
    }
}

/// Источник векторов для семантического индекса. Реализации несут формат
/// префиксов провайдера сами — вызывающая сторона знает только (title, text).
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Векторы документов: пара (title, text) — doc-структура retrieval-контракта.
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>>;
    /// Вектор поискового запроса (query-структура).
    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>>;
}

/// Doc-структура asymmetric retrieval (EmbeddingGemma 2): без тайтла — `none`.
pub fn doc_text(title: &str, text: &str) -> String {
    let t = if title.is_empty() { "none" } else { title };
    format!("title: {t} | text: {text}")
}

/// Query-структура: задача поиска зашита префиксом (task-steered модель).
pub fn query_text(query: &str) -> String {
    format!("task: search result | query: {query}")
}

pub struct OllamaEmbeddings {
    base_url: String,
    model: String,
    http: reqwest::Client,
    timeouts: ClientTimeouts,
}

impl OllamaEmbeddings {
    pub fn new(base_url: String, model: String) -> Self {
        Self::with_timeouts(base_url, model, ClientTimeouts::default())
    }

    /// Шов для тестов: короткие таймауты вместо production-значений.
    pub fn with_timeouts(base_url: String, model: String, timeouts: ClientTimeouts) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(timeouts.connect)
            .build()
            .expect("reqwest client");
        Self {
            base_url,
            model,
            http,
            timeouts,
        }
    }

    async fn embed_many(
        &self,
        texts: Vec<String>,
        total: Duration,
    ) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(embed_batch()) {
            out.extend(self.embed_one(chunk, total).await?);
        }
        Ok(out)
    }

    async fn embed_one(&self, texts: &[String], total: Duration) -> UsecaseResult<Vec<Vec<f32>>> {
        // Таймаут покрывает ВСЁ сетевое взаимодействие: send + заголовки +
        // тело ответа. stalling-body не должен держать поиск без верхней границы.
        let (status, body) = tokio::time::timeout(total, async {
            let resp = self
                .http
                .post(format!("{}/api/embed", self.base_url.trim_end_matches('/')))
                .json(&serde_json::json!({ "model": self.model, "input": texts }))
                .send()
                .await
                .map_err(|e| UsecaseError::State(format!("{SERVICE} transport: {e}")))?;
            let status = resp.status();
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| UsecaseError::State(format!("{SERVICE} body: {e}")))?;
            Ok::<_, UsecaseError>((status, body))
        })
        .await
        .map_err(|_| {
            UsecaseError::State(format!("{SERVICE}: request timeout after {total:?}"))
        })??;
        if !status.is_success() {
            return Err(UsecaseError::State(format!(
                "{SERVICE} HTTP {status}: {}",
                body.to_string().chars().take(160).collect::<String>()
            )));
        }
        let embs = body
            .get("embeddings")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                UsecaseError::State(format!("{SERVICE}: no embeddings in the response"))
            })?;
        if embs.len() != texts.len() {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: expected {} vectors, got {}",
                texts.len(),
                embs.len()
            )));
        }
        let vecs: Vec<Vec<f32>> = embs
            .iter()
            .map(|e| {
                e.as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: empty embedding")))
            })
            .collect::<UsecaseResult<Vec<_>>>()?;
        if let Some(v) = vecs.iter().find(|v| v.len() != search_dims()) {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: a {}-dim vector instead of {}",
                v.len(),
                search_dims()
            )));
        }
        Ok(vecs)
    }
}

#[async_trait]
impl Embedder for OllamaEmbeddings {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        let texts = docs.iter().map(|(t, c)| doc_text(t, c)).collect();
        self.embed_many(texts, self.timeouts.batch_total).await
    }

    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>> {
        self.embed_many(vec![query_text(query)], self.timeouts.query_total)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: empty response to the request")))
    }
}

/// Чанковка дока: абзацы группами ~1200 символов.
pub fn chunk_text(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for para in md.split("\n\n") {
        if buf.len() + para.len() > 1200 && !buf.is_empty() {
            out.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(para);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_prefix_formats_title() {
        assert_eq!(
            doc_text("Projects/x", "тело"),
            "title: Projects/x | text: тело"
        );
        assert_eq!(doc_text("", "тело"), "title: none | text: тело");
    }

    #[test]
    fn query_prefix_formats_task() {
        assert_eq!(
            query_text("лимиты крона"),
            "task: search result | query: лимиты крона"
        );
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }

    /// One-shot mock /api/embed: statusLine с выбранным Content-Length и
    /// тело (может быть короче объявленного — кейс «тело встало»).
    fn one_shot(status_line: &'static str, body: String, declared: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let mut got = 0usize;
            while got < buf.len() {
                let n = match sock.read(&mut buf[got..]) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }
                got += n;
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n{body}"
            );
            let _ = sock.write_all(resp.as_bytes());
            std::thread::sleep(std::time::Duration::from_millis(700));
        });
        format!("http://{addr}")
    }

    /// Обычные юниты работают на production-таймаутах: стаб держит сокет
    /// 700 мс после ответа, короткий бюджет здесь был бы источником флака.
    fn client(url: String) -> OllamaEmbeddings {
        OllamaEmbeddings::new(url, embed_model().to_string())
    }

    /// Только кейсы «ответа нет» получают короткий бюджет — иначе тест
    /// ждал бы production-таймаут.
    fn short_client(url: String) -> OllamaEmbeddings {
        OllamaEmbeddings::with_timeouts(
            url,
            embed_model().to_string(),
            ClientTimeouts {
                connect: Duration::from_millis(200),
                query_total: Duration::from_millis(200),
                batch_total: Duration::from_millis(200),
            },
        )
    }

    fn embed_body(n: usize, dim: usize) -> String {
        let vecs: Vec<_> = (0..n).map(|_| vec![0.0f32; dim]).collect();
        serde_json::json!({ "model": embed_model(), "embeddings": vecs }).to_string()
    }

    #[tokio::test]
    async fn success_batch_returns_768_dim_vectors() {
        let body = embed_body(2, search_dims());
        let len = body.len();
        let c = client(one_shot("HTTP/1.1 200 OK", body, len));
        let docs = vec![("T".into(), "x".into()), ("T2".into(), "y".into())];
        let out = c.embed_docs(&docs).await.expect("respond ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|v| v.len() == search_dims()));
    }

    #[tokio::test]
    async fn http_error_and_missing_embeddings_are_state_errors() {
        // JSON-тело: иначе разбор упадёт раньше проверки статуса и закрепит
        // не ту ветку — мутация статуса на Session останется невидимой.
        let body = r#"{"error":"boom"}"#.to_string();
        let len = body.len();
        let c = client(one_shot("HTTP/1.1 500 Internal Server Error", body, len));
        let err = c.embed_query("q").await.unwrap_err();
        assert!(
            matches!(&err, UsecaseError::State(m) if m.contains("HTTP 500")),
            "5xx -> State с кодом в тексте, факт: {err:?}"
        );

        let body = "{}".to_string();
        let len = body.len();
        let c = client(one_shot("HTTP/1.1 200 OK", body, len));
        let err = c.embed_query("q").await.unwrap_err().to_string();
        assert!(err.contains("no embeddings"), "fact: {err}");
    }

    #[tokio::test]
    async fn wrong_vector_count_and_dim_are_rejected() {
        // query шлёт один текст — два вектора в ответе должны отвергаться.
        let body = embed_body(2, search_dims());
        let len = body.len();
        let c = client(one_shot("HTTP/1.1 200 OK", body, len));
        let err = c.embed_query("q").await.unwrap_err().to_string();
        assert!(err.contains("expected 1 vectors, got"), "fact: {err}");

        let body = embed_body(1, 512);
        let len = body.len();
        let c = client(one_shot("HTTP/1.1 200 OK", body, len));
        let err = c.embed_query("q").await.unwrap_err().to_string();
        assert!(err.contains("512-dim vector"), "fact: {err}");
    }

    #[tokio::test]
    async fn stalled_body_fails_within_total_timeout() {
        // Заголовки пришли, рабочее тело короче объявленного Content-Length —
        // reqwest ждёт остаток; без таймаута на чтении вызов висел бы вечно.
        let body = embed_body(1, search_dims());
        let c = short_client(one_shot("HTTP/1.1 200 OK", body, 100_000));
        let started = std::time::Instant::now();
        let err = c.embed_query("q").await.unwrap_err().to_string();
        assert!(err.contains("timeout"), "fact: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "таймаут сработал"
        );
    }

    /// TCP принят, но ответа нет вообще (признак стейл-туннеля): бюджет
    /// должен оборвать ожидание, а не держать поиск-production-таймаут.
    #[tokio::test]
    async fn silent_server_fails_within_total_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::Read;
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf); // запрос прочитан, ответа нет
            std::thread::sleep(std::time::Duration::from_millis(1500));
        });
        let c = short_client(url);
        let started = std::time::Instant::now();
        let err = c.embed_query("q").await.unwrap_err().to_string();
        assert!(err.contains("timeout"), "fact: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "таймаут сработал"
        );
    }
}
