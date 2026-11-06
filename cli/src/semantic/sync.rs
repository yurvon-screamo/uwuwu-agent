//! Дельта-синхронизация и reconcile индекса против kernel SQL SiYuan.
//!
//! Корректностное ядро (задача 20261104072044, ревью-фиксы):
//! - keyset-пагинация по (updated, id): булк-импорт пишет тысячи блоков
//!   одной секундой (проба: топ-секунда миграции = 2190 блоков), строгий
//!   `updated >` терял бы хвосты run'а навсегда;
//! - overlap 60 s + идемпотентный upsert: граничные строки переезжают
//!   повторно, а не теряются (at-least-once);
//! - watermark двигается только по полностью записанному батчу — очередь
//!   дотягивается следующими запусками в рамках бюджета;
//! - каждый stmt к ядру несёт явные ORDER BY + LIMIT (без LIMIT ядро
//!   молча отдаёт первые 64 строки);
//! - reconcile: backfill пропущенных с эмбеддингом + prune исчезнувших
//!   (трэш исключён из kernel SQL — проба 05.11) + дрейф путей без
//!   переэмбеддинга.

use crate::siyuan::SiYuan;
use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

use super::ollama::OllamaClient;
use super::store::{ChunkMeta, ChunkRecord, Store};

/// Целевые типы блоков-чанков (контентные; l/d/av/tb — не чанки).
pub(crate) const TARGET_TYPES_SQL: &str = "('p','i','h','c','t')";
/// Страница kernel-выборки (явный LIMIT каждого stmt).
pub(crate) const PAGE: usize = 256;
/// Батч эмбеддинга.
pub(crate) const EMBED_BATCH: usize = 32;
/// Минимальная длина чанка (символы): заголовки-ярлыки не индексируем.
pub(crate) const MIN_CONTENT_CHARS: usize = 40;
/// Верхняя граница чанка (символы): html_embed-стены и крупные код-блоки
/// за ней дают эмбеддинг «головы» (ctx Ollama ~8192 токенов) — не индексируем.
/// Заземлено живым замером корпуса 06.11: выше границы 6 блоков из ~74k.
pub(crate) const MAX_CONTENT_CHARS: usize = 8000;
/// Overlap окна дельты: правки в ту же секунду, что watermark.
pub(crate) const OVERLAP_SECS: i64 = 60;
/// Reconcile не чаще раза в сутки.
pub(crate) const RECONCILE_INTERVAL_SECS: i64 = 24 * 3600;

pub struct SyncOptions {
    /// None → от watermark из meta минус overlap; Some(s) → стартовая граница
    /// (rebuild: Some("0") — с нуля).
    pub since: Option<String>,
    pub deadline: Option<Instant>,
    /// Пауза между kernel-страницами (rebuild): не душить прод-ядро
    /// keyset-сканами (поисковый путь — None, там 1–3 запроса в бюджете).
    pub page_delay: Option<Duration>,
    /// Прогресс-коллбэк после каждого записанного батча (счётчик embedded).
    pub on_batch: Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>,
}

#[derive(Default)]
pub struct SyncReport {
    pub embedded: usize,
    pub skipped_short: usize,
    /// Блоки за верхней границей длины — per-run счётчик (как skipped_short).
    pub skipped_long: usize,
    /// Строк, вычищенных из индекса (усохшие/переросшие чанки) — per-run.
    pub pruned: usize,
    /// Бюджет исчерпан до полной дотяжки — хвост останется над watermark.
    pub budget_exhausted: bool,
}

#[derive(Clone)]
pub struct KernelRow {
    pub meta: ChunkMeta,
    pub content: String,
}

pub(crate) fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Страница дельты: keyset по (updated, id), старейшие первыми.
/// Страница дельты: keyset по (updated, id), старейшие первыми. Лёгкие
/// колонки без content — полный скан blocks не таскает мегабайты текста
/// (ядро без индекса по updated; тяжёлый content достаётся точечно по
/// PK в fetch_by_ids — бережём прод-ядро от 13-минутных смертей).
pub(crate) fn delta_page_stmt(cursor_updated: &str, cursor_id: &str) -> String {
    format!(
        "select id, root_id, box, hpath, updated from blocks \
         where type in {types} \
         and (updated > {u} or (updated = {u} and id > {i})) \
         order by updated asc, id asc limit {page}",
        types = TARGET_TYPES_SQL,
        u = sql_literal(cursor_updated),
        i = sql_literal(cursor_id),
        page = PAGE,
    )
}

/// Класс длины контента: вне индексируемой полосы [MIN, MAX] чанк либо
/// ярлык (ниже нижней), либо «голова вместо содержимого» (за верхней).
enum LengthClass {
    TooShort,
    TooLong,
    Embeddable,
}

fn length_class(content: &str) -> LengthClass {
    let len = content.chars().count();
    if len <= MIN_CONTENT_CHARS {
        LengthClass::TooShort
    } else if len > MAX_CONTENT_CHARS {
        LengthClass::TooLong
    } else {
        LengthClass::Embeddable
    }
}

fn parse_kernel_row(row: &Value, with_content: bool) -> Option<KernelRow> {
    Some(KernelRow {
        meta: ChunkMeta {
            block_id: row.get("id")?.as_str()?.to_string(),
            root_id: row
                .get("root_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            box_id: row
                .get("box")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            hpath: row
                .get("hpath")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            updated: row
                .get("updated")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        content: if with_content {
            row.get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        },
    })
}

fn budget_left(deadline: Option<Instant>) -> bool {
    deadline.map(|d| Instant::now() < d).unwrap_or(true)
}

fn stored_dim(store: &Store) -> Option<usize> {
    store
        .meta("dim")
        .ok()
        .flatten()
        .and_then(|d| d.parse().ok())
}

/// Watermark минус overlap; некорректный формат — ошибка (мусор в meta).
fn since_with_overlap(watermark: &str) -> Result<String> {
    let ts = chrono::NaiveDateTime::parse_from_str(watermark, "%Y%m%d%H%M%S")
        .map_err(|e| anyhow::anyhow!("watermark «{watermark}» не парсится: {e}"))?;
    Ok((ts - chrono::Duration::seconds(OVERLAP_SECS))
        .format("%Y%m%d%H%M%S")
        .to_string())
}

/// Дельта-цикл: страницы → фильтр длины → эмбедд батчами → upsert+watermark.
///
/// EmbedError пробрасывается корнем anyhow-цепочки (search-путь даункастит
/// его в статус); kernel-ошибки — anyhow-контекст (маршрут ядра уже
/// отвечал полнотекстом).
pub async fn delta_sync(
    sy: &SiYuan,
    ollama: &OllamaClient,
    store: &mut Store,
    opts: &SyncOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let index_dim = stored_dim(store);

    let mut cursor_updated = match &opts.since {
        Some(since) => since.clone(),
        None => match store.meta("watermark")? {
            Some(mark) => since_with_overlap(&mark)?,
            None => "0".to_string(),
        },
    };
    let mut cursor_id = String::new();

    'pages: loop {
        if !budget_left(opts.deadline) {
            report.budget_exhausted = true;
            break;
        }
        let rows = sy
            .sql(&delta_page_stmt(&cursor_updated, &cursor_id))
            .await?;
        if rows.is_empty() {
            break;
        }
        let page_len = rows.len();
        // Точечный fetch content по PK: тяжёлый текст не проходит
        // через полный keyset-скан.
        let page_ids: Vec<String> = rows
            .iter()
            .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        let full_rows = fetch_by_ids(sy, &page_ids).await?;
        let by_id: std::collections::HashMap<String, KernelRow> = full_rows
            .into_iter()
            .map(|row| (row.meta.block_id.clone(), row))
            .collect();

        let mut pending: Vec<KernelRow> = Vec::with_capacity(EMBED_BATCH);
        let mut prune_ids: Vec<String> = Vec::new();
        let mut budget_out = false;
        for row in &rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(kernel_row) = by_id.get(id) else {
                // Блок исчез между сканом и fetch'ем — prune поймает reconcile'ом.
                continue;
            };
            cursor_updated = kernel_row.meta.updated.clone();
            cursor_id = kernel_row.meta.block_id.clone();
            match length_class(&kernel_row.content) {
                LengthClass::TooShort => {
                    report.skipped_short += 1;
                    prune_ids.push(kernel_row.meta.block_id.clone());
                }
                LengthClass::TooLong => {
                    report.skipped_long += 1;
                    prune_ids.push(kernel_row.meta.block_id.clone());
                }
                LengthClass::Embeddable => pending.push(kernel_row.clone()),
            }
            if pending.len() == EMBED_BATCH {
                if !budget_left(opts.deadline) {
                    report.budget_exhausted = true;
                    budget_out = true;
                    break;
                }
                embed_and_upsert(ollama, store, &pending, index_dim, &mut report).await?;
                if let Some(on_batch) = &opts.on_batch {
                    on_batch(report.embedded);
                }
                pending.clear();
            }
        }

        // Усохшие/переросшие чанки уходят из индекса в том же прогоне —
        // иначе дельта, пропустив строку, оставляла бы в индексе вечно
        // устаревший контент с вектором (п.1 ревью PR #78). DELETE
        // отсутствующих строк — no-op по PK; вернуть блок может правка.
        if !prune_ids.is_empty() {
            report.pruned += store.prune(&prune_ids)?;
        }
        if budget_out {
            break 'pages;
        }

        if !pending.is_empty() {
            embed_and_upsert(ollama, store, &pending, index_dim, &mut report).await?;
            if let Some(on_batch) = &opts.on_batch {
                on_batch(report.embedded);
            }
            pending.clear();
        }
        if page_len < PAGE {
            // Чистый конец: весь просмотренный диапазон обработан — watermark
            // на крайний курсор (включая пропущенные по длине строки);
            // запись — с MAX-гвардом (только вперёд).
            store.advance_watermark(&cursor_updated)?;
            break;
        }
        if let Some(delay) = opts.page_delay {
            tokio::time::sleep(delay).await;
        }
    }
    Ok(report)
}

/// Эмбедд батча + атомарная запись (чанки и watermark одной транзакцией).
/// Векторы приходят уже нормированными из клиента.
async fn embed_and_upsert(
    ollama: &OllamaClient,
    store: &mut Store,
    rows: &[KernelRow],
    index_dim: Option<usize>,
    report: &mut SyncReport,
) -> Result<()> {
    let inputs: Vec<String> = rows.iter().map(|r| r.content.clone()).collect();
    let vectors = ollama.embed(&inputs, index_dim).await?;
    if index_dim.is_none() {
        // Свежий индекс: первый ответ задаёт каноническую размерность.
        if let Some(first) = vectors.first() {
            store.set_meta("dim", &first.len().to_string())?;
        }
    }
    let records: Vec<ChunkRecord> = rows
        .iter()
        .zip(vectors)
        .map(|(row, vec)| ChunkRecord {
            meta: ChunkMeta {
                block_id: row.meta.block_id.clone(),
                root_id: row.meta.root_id.clone(),
                box_id: row.meta.box_id.clone(),
                hpath: row.meta.hpath.clone(),
                updated: row.meta.updated.clone(),
            },
            content: row.content.clone(),
            vec,
        })
        .collect();
    let watermark = rows.last().map(|r| r.meta.updated.clone());
    let tx = store.begin()?;
    Store::upsert_batch(&tx, &records, watermark.as_deref())?;
    tx.commit()?;
    report.embedded += records.len();
    Ok(())
}

#[derive(Default)]
pub struct ReconcileReport {
    /// Восстановлено строк (backfill пропущенных + переэмбедд изменённых).
    pub repaired: usize,
    pub pruned: usize,
    pub paths_refreshed: usize,
    /// Суточный интервал ещё не истёк — шаг пропущен.
    pub skipped: bool,
}

/// Полный id-diff ядра и индекса: самоисцеление пропусков (backfill),
/// prune исчезнувших (в т.ч. трэш — блоки удалённых доков выпадают из
/// kernel SQL), дрейф путей без переэмбеддинга. last_reconcile_at
/// ставится только по завершённому прогону — оборванный бюджет
/// повторится на следующем поиске.
pub async fn reconcile(
    sy: &SiYuan,
    ollama: &OllamaClient,
    store: &mut Store,
    deadline: Option<Instant>,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let now = chrono::Utc::now().timestamp();
    if let Some(last) = store
        .meta("last_reconcile_at")?
        .and_then(|v| v.parse::<i64>().ok())
    {
        if now - last < RECONCILE_INTERVAL_SECS {
            report.skipped = true;
            return Ok(report);
        }
    }

    // Лёгкий полный скан ядра: keyset по id, без content.
    let mut kernel: Vec<ChunkMeta> = Vec::new();
    let mut cursor = String::new();
    loop {
        if !budget_left(deadline) {
            return Ok(report);
        }
        let stmt = format!(
            "select id, root_id, box, hpath, updated from blocks \
             where type in {types} and id > {i} \
             order by id asc limit {page}",
            types = TARGET_TYPES_SQL,
            i = sql_literal(&cursor),
            page = PAGE,
        );
        let rows = sy.sql(&stmt).await?;
        let page_len = rows.len();
        for row in &rows {
            if let Some(parsed) = parse_kernel_row(row, false) {
                cursor = parsed.meta.block_id.clone();
                kernel.push(parsed.meta);
            }
        }
        if page_len < PAGE {
            break;
        }
    }

    let index = store.chunk_metas()?;
    let index_by_id: std::collections::HashMap<&str, &ChunkMeta> =
        index.iter().map(|m| (m.block_id.as_str(), m)).collect();
    let kernel_by_id: std::collections::HashMap<&str, &ChunkMeta> =
        kernel.iter().map(|m| (m.block_id.as_str(), m)).collect();

    // Три класса расхождений; порядок проверок важен: изменившийся updated
    // ремонтируется полным переэмбеддом (заберёт и свежий hpath).
    let mut repair_ids: Vec<String> = Vec::new();
    let mut path_drift: Vec<ChunkMeta> = Vec::new();
    for meta in &kernel {
        match index_by_id.get(meta.block_id.as_str()) {
            None => repair_ids.push(meta.block_id.clone()),
            Some(indexed) if indexed.updated != meta.updated => {
                repair_ids.push(meta.block_id.clone())
            }
            Some(indexed)
                if indexed.hpath != meta.hpath
                    || indexed.box_id != meta.box_id
                    || indexed.root_id != meta.root_id =>
            {
                path_drift.push(meta.clone())
            }
            _ => {}
        }
    }
    let vanished: Vec<String> = index_by_id
        .keys()
        .filter(|id| !kernel_by_id.contains_key(*id))
        .map(|id| id.to_string())
        .collect();

    if !vanished.is_empty() {
        report.pruned = store.prune(&vanished)?;
    }
    if !path_drift.is_empty() {
        report.paths_refreshed = store.refresh_paths(&path_drift)?;
    }

    let index_dim = stored_dim(store);
    for id_batch in repair_ids.chunks(PAGE) {
        if !budget_left(deadline) {
            return Ok(report);
        }
        let rows = fetch_by_ids(sy, id_batch).await?;
        // Классификация по длине: усохшие/переросшие блоки вычищаются из
        // индекса (а при backfill — просто не индексируются), полосу
        // [MIN, MAX] ремонтирует переэмбедд.
        let mut prune_ids: Vec<String> = Vec::new();
        let target: Vec<KernelRow> = rows
            .into_iter()
            .filter(|row| match length_class(&row.content) {
                LengthClass::Embeddable => true,
                LengthClass::TooShort | LengthClass::TooLong => {
                    prune_ids.push(row.meta.block_id.clone());
                    false
                }
            })
            .collect();
        if !prune_ids.is_empty() {
            report.pruned += store.prune(&prune_ids)?;
        }
        for batch in target.chunks(EMBED_BATCH) {
            if !budget_left(deadline) {
                return Ok(report);
            }
            let mut delta_report = SyncReport::default();
            embed_and_upsert(ollama, store, batch, index_dim, &mut delta_report).await?;
            report.repaired += delta_report.embedded;
        }
    }

    store.set_meta("last_reconcile_at", &now.to_string())?;
    Ok(report)
}

/// Точечный fetch полных строк по id (backfill-ремонт): явный LIMIT.
/// Строки, исчезнувшие между сканом и fetch'ем (трэш), тихо выпадают —
/// их уберёт следующий reconcile.
async fn fetch_by_ids(sy: &SiYuan, ids: &[String]) -> Result<Vec<KernelRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let literals = ids
        .iter()
        .map(|id| sql_literal(id))
        .collect::<Vec<_>>()
        .join(",");
    let stmt = format!(
        "select id, root_id, box, hpath, content, updated from blocks \
         where id in ({literals}) order by id asc limit {page}",
        page = ids.len(),
    );
    let rows = sy.sql(&stmt).await?;
    Ok(rows
        .iter()
        .filter_map(|row| parse_kernel_row(row, true))
        .collect())
}
