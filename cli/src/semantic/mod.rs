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

/// Параметры семантической половины: что искать и как резать выдачу.
pub struct SearchHalfRequest<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub threshold: f32,
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
    search_half_at(
        sy,
        cfg,
        &SearchHalfRequest {
            query,
            limit,
            threshold,
        },
        Instant::now() + SEARCH_BUDGET,
    )
    .await
}

/// Тестовый вход с явным дедлайном: остальной контракт тот же.
pub(crate) async fn search_half_at(
    sy: &SiYuan,
    cfg: &SemanticConfig,
    req: &SearchHalfRequest<'_>,
    deadline: Instant,
) -> Vec<String> {
    let header = format!(
        "Семантический ({model}, cos ≥ {threshold:.2}):",
        model = cfg.embed_model,
        threshold = req.threshold
    );

    if !cfg.db_path.exists() {
        return section(
            &header,
            format!("индекс пуст — {REBUILD_HINT} (~55–70 мин)"),
        );
    }
    let (mut store, ollama) = match open_ready_index(cfg) {
        Ok(ready) => ready,
        Err(status) => return section(&header, status),
    };

    // Query-first (задача 20261106182055): эмбедд запроса — первым, из
    // полного бюджета — результаты не заложники синхронизации.
    let query_vec = match query_embedding(&ollama, &store, req, deadline).await {
        Ok(vec) => vec,
        Err(status) => return section(&header, status),
    };

    // Синк — в остатке бюджета: смерть бюджета или отказ синка не подменяют
    // результаты, а становятся примечанием последней строкой секции.
    let sync_note = sync_in_leftover(sy, &ollama, &mut store, deadline).await;

    let hits = match store.knn(&query_vec, req.limit, req.threshold) {
        Ok(hits) => hits,
        Err(e) => return section(&header, format!("поиск по индексу: {e:#}")),
    };

    let mut lines = vec![header];
    if hits.is_empty() {
        lines.push(format!("ничего выше порога {:.2}", req.threshold));
    } else {
        for (i, hit) in hits.iter().enumerate() {
            lines.push(format!("{}. {}  ({:.2})", i + 1, hit.hpath, hit.score));
            lines.push(format!("    {}", hit.snippet.replace('\n', " ")));
        }
    }
    if let Some(note) = sync_note {
        lines.push(note);
    }
    lines
}

/// Эмбедд запроса из остатка бюджета; Err — готовый статус секции
/// (таймаут с фактом остатка, «недоступен» — прочие отказы маршрута).
async fn query_embedding(
    ollama: &OllamaClient,
    store: &Store,
    req: &SearchHalfRequest<'_>,
    deadline: Instant,
) -> Result<Vec<f32>, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(format!(
            "таймаут (бюджет {}s): бюджет израсходован синхронизацией, очередь дотянется следующими поисками",
            SEARCH_BUDGET.as_secs()
        ));
    }
    let index_dim = store
        .meta("dim")
        .ok()
        .flatten()
        .and_then(|d| d.parse().ok());
    match ollama
        .embed(&[req.query.to_string()], index_dim, remaining)
        .await
    {
        Ok(mut vectors) => Ok(ollama::normalize(vectors.remove(0))),
        Err(ollama::EmbedError::Timeout { .. }) => Err(format!(
            "таймаут (бюджет {}s): эмбедд запроса не уложился в остаток {:.1}s",
            SEARCH_BUDGET.as_secs(),
            remaining.as_secs_f32()
        )),
        Err(e) => Err(format!("недоступен — {e}")),
    }
}

/// Синк в остатке бюджета поиска: дельта первой (горячий путь), reconcile
/// следом (суточный guard внутри). None — дотянуться успели; Some —
/// примечание последней строкой секции: результаты уже посчитаны, отказ
/// синхронизации их не подменяет (query-first, задача 20261106182055).
async fn sync_in_leftover(
    sy: &SiYuan,
    ollama: &OllamaClient,
    store: &mut Store,
    deadline: Instant,
) -> Option<String> {
    let budget_note = "дельта не дотянулась в остатке бюджета — очередь дотянется следующими поисками";
    let report = match sync::delta_sync(
        sy,
        ollama,
        store,
        &sync::SyncOptions {
            since: None,
            deadline: Some(deadline),
            embed_timeout: SEARCH_EMBED_TIMEOUT,
            page_delay: None,
            on_batch: None,
        },
    )
    .await
    {
        Ok(report) => report,
        Err(e) => {
            return match e.downcast_ref::<ollama::EmbedError>() {
                Some(ollama::EmbedError::Timeout { .. }) => Some(budget_note.to_string()),
                _ => Some(format!("дельта не дотянулась: {e:#}")),
            };
        }
    };
    if report.budget_exhausted {
        return Some(budget_note.to_string());
    }
    if let Err(e) = sync::reconcile(sy, ollama, store, Some(deadline)).await {
        return Some(format!("reconcile не прошёл: {e:#}"));
    }
    None
}

/// Подготовить контекст семантической половины: открыть индекс, прогнать
/// guard'ы (пустой/чужая модель — деградация с хинтом), построить клиент
/// Ollama. Err — готовый статус секции.
fn open_ready_index(cfg: &SemanticConfig) -> Result<(Store, OllamaClient), String> {
    let store = Store::open(&cfg.db_path).map_err(|e| format!("хранилище недоступно: {e:#}"))?;
    index_ready(&store, cfg)?;
    let ollama = OllamaClient::new(&cfg.embed_url, &cfg.embed_model, SEARCH_EMBED_TIMEOUT)
        .map_err(|e| format!("клиент Ollama: {e:#}"))?;
    Ok((store, ollama))
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

pub struct RebuildReport {
    pub embedded: usize,
    pub skipped_short: usize,
    pub elapsed: Duration,
}

/// Полный пересбор индекса: сброс чанков/меты + дельта-цикл с нуля;
/// `resume` — продолжение прерванного прогона от watermark без сброса.
/// Прогресс — коллбэк после каждого батча (CLI печатает в stderr).
/// Advisory-лок держится весь прогон (acquire до ветки resume/clear,
/// release на любом исходе) — параллельный rebuild громко отказывает.
pub async fn rebuild(
    sy: &SiYuan,
    cfg: &SemanticConfig,
    on_batch: Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>,
    resume: bool,
) -> Result<RebuildReport> {
    let mut store = Store::open(&cfg.db_path)?;
    store.acquire_rebuild_lock()?;
    let result = rebuild_index(sy, cfg, &mut store, on_batch, resume).await;
    // Крэш-лок (убитый процесс) снимается steal'ом следующего запуска.
    let _ = store.release_rebuild_lock();
    result
}

async fn rebuild_index(
    sy: &SiYuan,
    cfg: &SemanticConfig,
    store: &mut Store,
    on_batch: Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>,
    resume: bool,
) -> Result<RebuildReport> {
    let started = Instant::now();
    if resume {
        if !cfg.db_path.exists() || store.chunk_count().unwrap_or(0) == 0 {
            anyhow::bail!("--resume: индекс пуст — продолжать нечего, запускай без --resume");
        }
        // Model-guard resume (п.3 ревью PR #78): guard на поисковом пути
        // не спасает resume — тот молча дособрал бы индекс смешанными
        // эмбеддингами двух моделей.
        match store.meta("model")? {
            Some(indexed) if indexed != cfg.embed_model => anyhow::bail!(
                "--resume: индекс собран моделью «{indexed}», env — «{env}» — продолжение смешает эмбеддинги; запускай без --resume (полный пересбор)",
                env = cfg.embed_model
            ),
            // Сбой до первого батча: модель не успела записаться — adopt.
            None => store.set_meta("model", &cfg.embed_model)?,
            _ => {}
        }
    } else {
        store.clear_for_rebuild()?;
        store.set_meta("model", &cfg.embed_model)?;
    }

    let ollama = OllamaClient::new(&cfg.embed_url, &cfg.embed_model, REBUILD_EMBED_TIMEOUT)?;
    let opts = sync::SyncOptions {
        since: if resume { None } else { Some("0".to_string()) },
        deadline: None,
        embed_timeout: REBUILD_EMBED_TIMEOUT,
        page_delay: Some(Duration::from_millis(200)),
        on_batch,
    };
    let report = sync::delta_sync(sy, &ollama, store, &opts).await?;
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
    let oversize = oversize_count(sy).await?;
    Ok(vec![
        format!("чанков: {count}"),
        format!("модель: {model} ({dim}-dim)"),
        format!("watermark: {watermark}"),
        format!("built: {built_at}, reconcile: {reconciled}"),
        format!("очередь дельты: {backlog} строк"),
        format!(
            "блоков >{} симв. в корпусе: {oversize} (не индексируются)",
            sync::MAX_CONTENT_CHARS
        ),
        format!("путь: {}", cfg.db_path.display()),
    ])
}

fn human_ts(raw: Option<String>) -> String {
    raw.and_then(|v| v.parse::<i64>().ok())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "—".into())
}

/// Тестовый вход в backlog_size (иначе приватен для semantic_tests).
#[cfg(test)]
pub(crate) async fn status_backlog_for_tests(sy: &SiYuan, store: &Store) -> u64 {
    backlog_size(sy, store).await.expect("backlog")
}

/// Живая state-метрика корпуса: блоки за верхней границей длины, по целевым
/// типам — тот же фильтр, что у индексации. Пересчёт на каждый status —
/// overlap-перескан дельты накопительный счётчик не завышает по построению.
async fn oversize_count(sy: &SiYuan) -> Result<u64> {
    let stmt = format!(
        "select count(*) as c from blocks \
         where type in {types} and length(content) > {max} limit 1",
        types = sync::TARGET_TYPES_SQL,
        max = sync::MAX_CONTENT_CHARS,
    );
    let rows = sy.sql(&stmt).await?;
    Ok(rows
        .first()
        .and_then(|r| r.get("c"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0) as u64)
}

/// Размер очереди: строки ядра над watermark-парой (без content; явный
/// LIMIT). Пара курсора: `updated > wu or (updated = wu и id > wi)` —
/// хвост лавины одной секунды виден, закоммиченный префикс секунды — нет.
async fn backlog_size(sy: &SiYuan, store: &Store) -> Result<u64> {
    let Some(mark) = store.meta("watermark")? else {
        return Ok(0);
    };
    let (mark_updated, mark_id) = sync::split_watermark(&mark);
    let stmt = format!(
        "select count(*) as c from blocks \
         where type in {types} \
         and (updated > {u} or (updated = {u} and id > {i})) limit 1",
        types = sync::TARGET_TYPES_SQL,
        u = sync::sql_literal(&mark_updated),
        i = sync::sql_literal(&mark_id),
    );
    let rows = sy.sql(&stmt).await?;
    Ok(rows
        .first()
        .and_then(|r| r.get("c"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0) as u64)
}
