//! Semantic-индекс uwuwu-cli: локальные эмбеддинги Ollama по блокам SiYuan.
//!
//! Always-on dual search (решение владельца 05.11): каждый `uwuwu-cli search`
//! отвечает полнотекстом и семантикой — top-n по каждому методу. Семантическая
//! половина никогда не роняет команду: любой отказ маршрута (Ollama лежит,
//! модели нет, смена модели/размерности, пустой индекс, таймаут бюджета)
//! становится видимой строкой статуса секции — «тихо слепой» поиск исключён
//! конструктивно. Обслуживание индекса — `uwuwu-cli semantic rebuild|status`
//! (bootstrap пустого индекса — только явной командой, не внутри поиска).

pub mod ollama;
pub mod store;
pub mod sync;

use crate::siyuan::SiYuan;
use anyhow::Result;
use ollama::OllamaClient;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use store::Store;

/// Wall-clock бюджет семантической половины одного поиска (reconcile-проба +
/// дельта + эмбедд запроса + KNN). Холодный старт модели ~5.5 s вмещается;
/// превышение — деградация со статусом.
pub(crate) const SEARCH_BUDGET: Duration = Duration::from_secs(10);
/// Таймаут батча на поисковом пути; `semantic rebuild` строит клиент шире.
pub(crate) const SEARCH_EMBED_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const REBUILD_EMBED_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const DEFAULT_EMBED_URL: &str = "http://127.0.0.1:11434";
pub(crate) const DEFAULT_EMBED_MODEL: &str = "qwen3-embedding:0.6b";
const REBUILD_HINT: &str = "uwuwu-cli semantic rebuild";

pub struct SemanticConfig {
    pub embed_url: String,
    pub embed_model: String,
    pub db_path: PathBuf,
}

impl SemanticConfig {
    pub fn from_env() -> Self {
        let embed_url = std::env::var("UWUWU_EMBED_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_EMBED_URL.to_string());
        let embed_model = std::env::var("UWUWU_EMBED_MODEL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string());
        let db_path = std::env::var("UWUWU_SEMANTIC_DB")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(default_db_path);
        Self {
            embed_url,
            embed_model,
            db_path,
        }
    }
}

fn default_db_path() -> PathBuf {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        });
    data_home.join("uwuwu-cli").join("semantic.db")
}

/// Строки семантической секции ответа search: заголовок + выдача либо
/// объяснение деградации (статус всегда видим в stdout). Печать — вызывающий.
pub async fn search_half(
    sy: &SiYuan,
    cfg: &SemanticConfig,
    query: &str,
    limit: usize,
    threshold: f32,
) -> Vec<String> {
    let header = format!(
        "Семантический ({model}, cos ≥ {threshold:.2}):",
        model = cfg.embed_model
    );
    let deadline = Instant::now() + SEARCH_BUDGET;

    if !cfg.db_path.exists() {
        return section(
            &header,
            format!("индекс пуст — {REBUILD_HINT} (~55–70 мин)"),
        );
    }
    let mut store = match Store::open(&cfg.db_path) {
        Ok(store) => store,
        Err(e) => return section(&header, format!("хранилище недоступно: {e:#}")),
    };
    if let Err(status) = index_ready(&store, cfg) {
        return section(&header, status);
    }

    let ollama = match OllamaClient::new(&cfg.embed_url, &cfg.embed_model, SEARCH_EMBED_TIMEOUT) {
        Ok(client) => client,
        Err(e) => return section(&header, format!("клиент Ollama: {e:#}")),
    };

    if let Err(e) = sync::reconcile(sy, &ollama, &mut store, Some(deadline)).await {
        return section(&header, route_error(&e));
    }
    let report = match sync::delta_sync(
        sy,
        &ollama,
        &mut store,
        &sync::SyncOptions {
            since: None,
            deadline: Some(deadline),
            on_batch: None,
        },
    )
    .await
    {
        Ok(report) => report,
        Err(e) => return section(&header, route_error(&e)),
    };

    let index_dim = store
        .meta("dim")
        .ok()
        .flatten()
        .and_then(|d| d.parse().ok());
    let query_vec = match ollama.embed(&[query.to_string()], index_dim).await {
        Ok(mut vectors) => ollama::normalize(vectors.remove(0)),
        Err(e) => return section(&header, format!("недоступен — {e}")),
    };
    let hits = match store.knn(&query_vec, limit, threshold) {
        Ok(hits) => hits,
        Err(e) => return section(&header, format!("поиск по индексу: {e:#}")),
    };

    let mut lines = vec![header];
    if hits.is_empty() {
        lines.push(format!("ничего выше порога {threshold:.2}"));
    } else {
        for (i, hit) in hits.iter().enumerate() {
            lines.push(format!("{}. {}  ({:.2})", i + 1, hit.hpath, hit.score));
            lines.push(format!("    {}", hit.snippet.replace('\n', " ")));
        }
    }
    if report.budget_exhausted {
        lines.push(
            "(индекс дотянут частично: бюджет исчерпан — очередь дотянется следующими поисками)"
                .to_string(),
        );
    }
    lines
}

/// Guard'ы перед дельтой: пустой индекс и смена модели — деградация
/// с хинтом rebuild, смешение эмбеддингов двух моделей исключено.
/// (dim-guard срабатывает в самом embed-ответе.)
fn index_ready(store: &Store, cfg: &SemanticConfig) -> Result<(), String> {
    if store.chunk_count().unwrap_or(0) == 0 {
        return Err(format!("индекс пуст — {REBUILD_HINT} (~55–70 мин)"));
    }
    match store.meta("model").ok().flatten() {
        Some(indexed) if indexed != cfg.embed_model => Err(format!(
            "индекс собран другой моделью («{indexed}», env — «{model}») — {REBUILD_HINT}",
            model = cfg.embed_model
        )),
        _ => Ok(()),
    }
}

fn section(header: &str, status: String) -> Vec<String> {
    vec![header.to_string(), status]
}

fn route_error(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ollama::EmbedError>() {
        Some(embed_error) => format!("недоступен — {embed_error}"),
        None => format!("сбой синхронизации: {error:#}"),
    }
}

pub struct RebuildReport {
    pub embedded: usize,
    pub skipped_short: usize,
    pub elapsed: Duration,
}

/// Полный пересбор индекса: сброс чанков/меты + дельта-цикл с нуля.
/// Прогресс — коллбэк после каждого батча (CLI печатает в stderr).
pub async fn rebuild(
    sy: &SiYuan,
    cfg: &SemanticConfig,
    on_batch: Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>,
) -> Result<RebuildReport> {
    let started = Instant::now();
    let mut store = Store::open(&cfg.db_path)?;
    store.clear_for_rebuild()?;
    store.set_meta("model", &cfg.embed_model)?;

    let ollama = OllamaClient::new(&cfg.embed_url, &cfg.embed_model, REBUILD_EMBED_TIMEOUT)?;
    let opts = sync::SyncOptions {
        since: Some("0".to_string()),
        deadline: None,
        on_batch,
    };
    let report = sync::delta_sync(sy, &ollama, &mut store, &opts).await?;
    store.set_meta("built_at", &chrono::Utc::now().timestamp().to_string())?;
    Ok(RebuildReport {
        embedded: report.embedded,
        skipped_short: report.skipped_short,
        elapsed: started.elapsed(),
    })
}

/// Строки `uwuwu-cli semantic status`: состояние индекса и очереди дельты.
pub async fn status(sy: &SiYuan, cfg: &SemanticConfig) -> Result<Vec<String>> {
    if !cfg.db_path.exists() {
        return Ok(vec![
            "индекс: отсутствует (пуст)".to_string(),
            format!("путь: {}", cfg.db_path.display()),
            format!("индекс пуст — {REBUILD_HINT} (~55–70 мин)"),
        ]);
    }
    let store = Store::open(&cfg.db_path)?;
    let count = store.chunk_count()?;
    let model = store.meta("model")?.unwrap_or_else(|| "—".into());
    let dim = store.meta("dim")?.unwrap_or_else(|| "—".into());
    let watermark = store.meta("watermark")?.unwrap_or_else(|| "—".into());
    let built_at = human_ts(store.meta("built_at")?);
    let reconciled = human_ts(store.meta("last_reconcile_at")?);
    let backlog = backlog_size(sy, &store).await?;
    Ok(vec![
        format!("чанков: {count}"),
        format!("модель: {model} ({dim}-dim)"),
        format!("watermark: {watermark}"),
        format!("built: {built_at}, reconcile: {reconciled}"),
        format!("очередь дельты: {backlog} строк"),
        format!("путь: {}", cfg.db_path.display()),
    ])
}

fn human_ts(raw: Option<String>) -> String {
    raw.and_then(|v| v.parse::<i64>().ok())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "—".into())
}

/// Размер очереди: строки ядра над watermark (без content; явный LIMIT).
async fn backlog_size(sy: &SiYuan, store: &Store) -> Result<u64> {
    let Some(mark) = store.meta("watermark")? else {
        return Ok(0);
    };
    let stmt = format!(
        "select count(*) as c from blocks \
         where type in {types} and updated > {u} limit 1",
        types = sync::TARGET_TYPES_SQL,
        u = mark.replace('\'', "''"),
    );
    let rows = sy.sql(&stmt).await?;
    Ok(rows
        .first()
        .and_then(|r| r.get("c"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0) as u64)
}
