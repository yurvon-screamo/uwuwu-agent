//! Semantic-индекс uwuwu-cli: локальные эмбеддинги Ollama по блокам SiYuan.
//!
//! Always-on dual search (решение владельца 05.11): каждый `uwuwu-cli search`
//! отвечает полнотекстом и семантикой — top-n по каждому методу. Семантическая
//! половина никогда не роняет команду: любой отказ маршрута (Ollama лежит,
//! модели нет, смена модели/размерности, пустой индекс, таймаут бюджета)
//! становится видимой строкой статуса секции — «тихо слепой» поиск исключён
//! конструктивно. Обслуживание индекса — `uwuwu-cli semantic rebuild|status`
//! (bootstrap пустого индекса — только явной командой, не внутри поиска).

pub mod hub_source;
pub mod hub_sync;
pub mod ollama;
pub mod store;
pub mod sync;

use crate::store::github_doc_store::GitHubDocStore;
use anyhow::Result;
use ollama::OllamaClient;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use store::Store;

/// Hub-режим semantic-индекса: UWUWU_HUB_REPO задан → скан uwuwu-hub
/// (docs md из git-клона + issues через gh API) вместо kernel-дельты.
fn hub_doc_store() -> Option<GitHubDocStore> {
    match std::env::var("UWUWU_HUB_REPO") {
        Ok(repo) if !repo.is_empty() => Some(GitHubDocStore::from_env(repo)),
        _ => None,
    }
}

/// Wall-clock бюджет семантической половины одного поиска (дельта +
/// эмбедд запроса + KNN; reconcile на поиске не ходит — см. delta_in_leftover).
/// Холодный старт модели ~5.5 s вмещается; превышение — деградация со статусом.
pub(crate) const SEARCH_BUDGET: Duration = Duration::from_secs(10);
/// Таймаут батча на поисковом пути; `semantic rebuild` строит клиент шире.
pub(crate) const SEARCH_EMBED_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const REBUILD_EMBED_TIMEOUT: Duration = Duration::from_secs(120);
/// Дефолт бюджета ручной дотяжки `semantic sync`: очередь в тысячи строк ≈
/// минуты тёплых батчей (1879 строк ≈ 60 батчей ≈ 2–5 мин) с запасом;
/// команда явная — долгий wall-clock приемлем. Хвост над watermark догоняется
/// повтором команды. Override для разового бэкфилла — UWUWU_SYNC_BUDGET_SECS.
pub(crate) const SYNC_BUDGET: Duration = Duration::from_secs(600);
/// Порог «давний reconcile» в `semantic status`: самопочинка индекса живёт
/// в явном `semantic sync`, возраст прогона виден здесь.
const RECONCILE_STALE_SECS: i64 = 24 * 3600;
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
    cfg: &SemanticConfig,
    query: &str,
    limit: usize,
    threshold: f32,
) -> Vec<String> {
    search_half_at(
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

    // Дельта — в остатке бюджета: смерть бюджета или отказ дельты не
    // подменяют результаты, а становятся примечанием последней строкой
    // секции. Reconcile на поиске не ходит (delta_in_leftover).
    let sync_note = delta_in_leftover(&ollama, &mut store, deadline).await;

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

/// Эмбедд запроса — первым, из полного бюджета (query-first: sync идёт
/// после и не может исчерпать бюджет до запроса); Err — готовый статус
/// секции (таймаут с фактом остатка, «недоступен» — прочие отказы маршрута).
async fn query_embedding(
    ollama: &OllamaClient,
    store: &Store,
    req: &SearchHalfRequest<'_>,
    deadline: Instant,
) -> Result<Vec<f32>, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        // Живой поиск создаёт дедлайн now+10s прямо перед вызовом — ветка
        // достижима только при внешнем вызове с уже истёкшим дедлайном.
        return Err(format!(
            "таймаут (бюджет {}s): остаток бюджета нулевой на старте — эмбедд запроса не выполнялся",
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

/// Дельта в остатке бюджета поиска: единственная синхронизация поискового
/// пути. Reconcile (полный id-скан корпуса + repair-эмбедды) в 10-секундный
/// остаток структурно не помещается — попытки на поиске давали хронный
/// «reconcile не прошёл» и ~550 SQL-запросов скана на каждый поиск
/// (задача 20261112203959); он живёт в `semantic sync` (SYNC_BUDGET,
/// REBUILD_EMBED_TIMEOUT). None — дотянуться успели; Some — примечание
/// последней строкой секции: результаты уже посчитаны, отказ синхронизации
/// их не подменяет (query-first, задача 20261106182055).
async fn delta_in_leftover(
    ollama: &OllamaClient,
    store: &mut Store,
    deadline: Instant,
) -> Option<String> {
    let budget_note =
        "дельта не дотянулась в остатке бюджета — очередь дотянется следующими поисками";
    let doc = hub_doc_store()?;
    let report = match hub_sync::hub_sync(
        &doc,
        ollama,
        store,
        &sync::SyncOptions {
            deadline: Some(deadline),
            embed_timeout: SEARCH_EMBED_TIMEOUT,
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
    cfg: &SemanticConfig,
    on_batch: Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>,
    resume: bool,
) -> Result<RebuildReport> {
    let mut store = Store::open(&cfg.db_path)?;
    store.acquire_rebuild_lock()?;
    let result = rebuild_index(cfg, &mut store, on_batch, resume).await;
    // Крэш-лок (убитый процесс) снимается steal'ом следующего запуска.
    let _ = store.release_rebuild_lock();
    result
}

async fn rebuild_index(
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
        deadline: None,
        embed_timeout: REBUILD_EMBED_TIMEOUT,
        // hub-скан ходит в свой git-клон и gh API — ядро не душит
        on_batch,
    };
    let doc = hub_doc_store().ok_or_else(|| {
        anyhow::anyhow!("semantic: UWUWU_HUB_REPO не задан — hub-источник обязателен")
    })?;
    let report = hub_sync::hub_sync(&doc, &ollama, store, &opts).await?;
    let now = chrono::Utc::now().timestamp();
    store.set_meta("built_at", &now.to_string())?;
    if !resume {
        // Полный rebuild = завершённая сверка с корпусом (проход от нуля
        // встроил всю полосу): штампуем last_reconcile_at, чтобы status не
        // показывал ложное «reconcile давний» сразу после пересборки.
        // --resume идёт от watermark — сверкой не является, не штампуем.
        store.set_meta("last_reconcile_at", &now.to_string())?;
    }
    Ok(RebuildReport {
        embedded: report.embedded,
        skipped_short: report.skipped_short,
        elapsed: started.elapsed(),
    })
}

/// Ручная дотяжка индекса вне бюджета поиска (`semantic sync`): дельта,
/// затем reconcile (полный id-скан + repair) — с advisory-локом
/// (параллельный rebuild или синк громко отказывает: сотни секунд записей
/// без лока могли бы портить чужой свежий rebuild — watermark перескочил бы
/// незакрытые строки). Строки отчёта — в stdout вызывающего.
pub async fn sync_once(cfg: &SemanticConfig) -> Result<Vec<String>> {
    if !cfg.db_path.exists() {
        anyhow::bail!("индекс пуст — {REBUILD_HINT}");
    }
    let mut store = Store::open(&cfg.db_path)?;
    index_ready(&store, cfg).map_err(anyhow::Error::msg)?;
    store.acquire_rebuild_lock()?;
    let result = sync_once_locked(cfg, &mut store).await;
    // Крэш-лок снимается steal'ом следующего запуска (как у rebuild).
    let _ = store.release_rebuild_lock();
    result
}

async fn sync_once_locked(cfg: &SemanticConfig, store: &mut Store) -> Result<Vec<String>> {
    let ollama = OllamaClient::new(&cfg.embed_url, &cfg.embed_model, REBUILD_EMBED_TIMEOUT)?;
    let deadline = Instant::now() + sync_budget();
    let doc =
        hub_doc_store().ok_or_else(|| anyhow::anyhow!("semantic: UWUWU_HUB_REPO не задан"))?;
    let report = hub_sync::hub_sync(
        &doc,
        &ollama,
        store,
        &sync::SyncOptions {
            deadline: Some(deadline),
            embed_timeout: REBUILD_EMBED_TIMEOUT,
            on_batch: None,
        },
    )
    .await?;
    let lines = vec![
        format!(
            "hub-скан: {} чанков (коротких пропущено {}, вычищено {}, путей освежено {})",
            report.embedded, report.skipped_short, report.pruned, report.paths_refreshed,
        ),
        "backlog: — (hub: полный скан дедупится содержимым, watermark нет)".to_string(),
    ];
    Ok(lines)
}

/// Бюджет `semantic sync`: дефолт SYNC_BUDGET, override — UWUWU_SYNC_BUDGET_SECS
/// (разовый бэкфилл дрейфа на десятки минут; мусор/ноль в env — дефолт).
fn sync_budget() -> Duration {
    std::env::var("UWUWU_SYNC_BUDGET_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map_or(SYNC_BUDGET, Duration::from_secs)
}

/// Строки `uwuwu-cli semantic status`: состояние индекса и очереди дельты.
pub async fn status(cfg: &SemanticConfig) -> Result<Vec<String>> {
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
    let mut lines = vec![
        format!("чанков: {count}"),
        format!("модель: {model} ({dim}-dim)"),
        format!("watermark: {watermark}"),
        format!("built: {built_at}, reconcile: {reconciled} (hub: полный скан)"),
    ];
    if let Some(hint) = reconcile_stale_hint(store.meta("last_reconcile_at")?) {
        lines.push(hint);
    }
    lines.extend([format!("путь: {}", cfg.db_path.display())]);
    Ok(lines)
}

/// Подсказка о давнем reconcile: самопочинка индекса живёт в явном
/// `semantic sync`, возраст последнего завершённого прогона виден здесь.
/// Отсутствие меты (reconcile ещё не был) — тоже давний.
fn reconcile_stale_hint(raw: Option<String>) -> Option<String> {
    let now = chrono::Utc::now().timestamp();
    let fresh = raw
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|last| now - last <= RECONCILE_STALE_SECS);
    if fresh {
        None
    } else {
        Some("reconcile давний — прогони uwuwu-cli semantic sync".to_string())
    }
}

fn human_ts(raw: Option<String>) -> String {
    raw.and_then(|v| v.parse::<i64>().ok())
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "—".into())
}
