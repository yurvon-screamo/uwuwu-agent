//! Общий конвейер синка semantic-индекса: классификация длины, окна
//! эмбеддинга, process_page (skip-unchanged/prune/drift), батчи с
//! бюджет-гейтом. Источники строк — hub (hub_sync.rs, срез замены бекенда
//! 20261112155302-ttyp35g); kernel-дельта SiYuan вырезана (срез F).

use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

use super::ollama::OllamaClient;
use super::store::{ChunkMeta, ChunkRecord, IndexedRow, Store};

/// Батч эмбеддинга: потолок строк.
pub(crate) const EMBED_BATCH: usize = 32;
/// Потолок суммарных символов одного embed-батча: эмбедд ~5k симв./сек
/// (живой замер 12.11: 32×4400 ≈ 28 с), батч обязан умещаться в таймаут
/// пути с запасом — жирные чанки режутся в отдельные окна. Полоса чанков
/// ограничена MAX_CONTENT_CHARS (8000), поэтому одиночный чанк всегда
/// укладывается в бюджет окна.
pub(crate) const EMBED_BATCH_CHARS: usize = 24_000;
/// Минимальная длина чанка (символы): заголовки-ярлыки не индексируем.
pub(crate) const MIN_CONTENT_CHARS: usize = 40;
/// Верхняя граница чанка (символы): стены кода дают эмбеддинг «головы» —
/// не индексируем. Заземлено живым замером корпуса 06.11: выше границы
/// 6 блоков из ~74k.
pub(crate) const MAX_CONTENT_CHARS: usize = 8000;

pub struct SyncOptions {
    pub deadline: Option<Instant>,
    /// Потолок таймаута одного embed-вызова (rebuild — 120s, поиск —
    /// SEARCH_EMBED_TIMEOUT); фактический таймаут = min(cap, остаток deadline).
    pub embed_timeout: Duration,
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
    /// Дрейф путей, освежённый без переэмбедда (hpath/box/root при
    /// неизменных updated и content).
    pub paths_refreshed: usize,
    /// Бюджет исчерпан до полной дотяжки.
    pub budget_exhausted: bool,
}

/// Строка источника: мета чанка + контент.
#[derive(Clone, Debug)]
pub struct KernelRow {
    pub meta: ChunkMeta,
    pub content: String,
}

/// Окна эмбеддинга: слайсы набора строк по EMBED_BATCH строк И по
/// EMBED_BATCH_CHARS суммарных символов — набор длинных чанков не собирается
/// в батч, чей embed дольше таймаута пути.
pub(crate) fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn embed_batch_windows(rows: &[KernelRow]) -> Vec<&[KernelRow]> {
    let mut windows = Vec::new();
    let mut start = 0usize;
    let mut window_chars = 0usize;
    for (i, row) in rows.iter().enumerate() {
        let chars = row.content.chars().count();
        let full_by_count = i - start >= EMBED_BATCH;
        let full_by_chars = window_chars > 0 && window_chars + chars > EMBED_BATCH_CHARS;
        if full_by_count || full_by_chars {
            windows.push(&rows[start..i]);
            start = i;
            window_chars = 0;
        }
        window_chars += chars;
    }
    if start < rows.len() {
        windows.push(&rows[start..]);
    }
    windows
}

/// Класс длины контента: вне индексируемой полосы [MIN, MAX] чанк либо
/// ярлык (ниже нижней), либо «голова вместо содержимого» (за верхней).
enum LengthClass {
    Embeddable,
    TooShort,
    TooLong,
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

fn embed_call_timeout(deadline: Option<Instant>, cap: Duration) -> Duration {
    match deadline {
        Some(deadline) => cap.min(deadline.saturating_duration_since(Instant::now())),
        None => cap,
    }
}

fn stored_dim(store: &Store) -> Option<usize> {
    store
        .meta("dim")
        .ok()
        .flatten()
        .and_then(|d| d.parse().ok())
}

/// Результат классификации страницы дельты (чистый, без IO).
pub(crate) struct PageOutcome {
    /// Строки полосы [MIN, MAX] — кандидаты на эмбедд.
    pub(crate) pending: Vec<KernelRow>,
    /// Усохшие/переросшие строки — prune-кандидаты (DELETE отсутствующих
    /// no-op по PK; вернуть блок может правка).
    pub(crate) prune_ids: Vec<String>,
    /// Дрейф метаданных (hpath/box/root) при неизменных (updated, content) —
    /// освежается без переэмбедда.
    pub(crate) drift: Vec<ChunkMeta>,
    pub(crate) skipped_short: usize,
    pub(crate) skipped_long: usize,
}

/// Классификация строк дельты: строка, уже проиндексированная с теми же
/// (updated, content), — чистый skip: повторный скан не переэмбеддит
/// идемпотентное (анти-карусель 20261106182055), расхождение только в
/// путях — дрейф без эмбедда.
pub(crate) fn process_page(
    rows: &[Value],
    by_id: &std::collections::HashMap<String, KernelRow>,
    indexed: &std::collections::HashMap<String, IndexedRow>,
) -> PageOutcome {
    let mut outcome = PageOutcome {
        pending: Vec::with_capacity(rows.len()),
        prune_ids: Vec::new(),
        drift: Vec::new(),
        skipped_short: 0,
        skipped_long: 0,
    };
    for row in rows {
        let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(kernel_row) = by_id.get(id) else {
            continue;
        };
        if let Some(already) = indexed.get(id) {
            if already.meta.updated == kernel_row.meta.updated
                && already.content == kernel_row.content
            {
                let drifted = already.meta.hpath != kernel_row.meta.hpath
                    || already.meta.box_id != kernel_row.meta.box_id
                    || already.meta.root_id != kernel_row.meta.root_id;
                if drifted {
                    outcome.drift.push(kernel_row.meta.clone());
                }
                continue;
            }
        }
        match length_class(&kernel_row.content) {
            LengthClass::TooShort => {
                outcome.skipped_short += 1;
                outcome.prune_ids.push(kernel_row.meta.block_id.clone());
            }
            LengthClass::TooLong => {
                outcome.skipped_long += 1;
                outcome.prune_ids.push(kernel_row.meta.block_id.clone());
            }
            LengthClass::Embeddable => outcome.pending.push(kernel_row.clone()),
        }
    }
    outcome
}

/// Эмбедд батчей с бюджет-гейтом на каждый батч; false — бюджет исчерпан.
/// Таймаут, урезанный бюджетом (меньше потолка пути), — это исчерпание
/// бюджета посреди эмбедда, а не отказ сервера: грациозная остановка
/// вместо Err. Таймаут при полном потолке — настоящий шторм ollama, Err.
pub(crate) async fn embed_batches(
    ollama: &OllamaClient,
    store: &mut Store,
    batches: &[&[KernelRow]],
    opts: &SyncOptions,
    report: &mut SyncReport,
) -> Result<bool> {
    for batch in batches {
        let timeout = embed_call_timeout(opts.deadline, opts.embed_timeout);
        if timeout.is_zero() {
            report.budget_exhausted = true;
            return Ok(false);
        }
        if let Err(e) = embed_and_upsert(ollama, store, batch, timeout, report).await {
            if timeout < opts.embed_timeout && is_embed_timeout(&e) {
                report.budget_exhausted = true;
                return Ok(false);
            }
            return Err(e);
        }
        if let Some(on_batch) = &opts.on_batch {
            on_batch(report.embedded);
        }
    }
    Ok(true)
}

/// Таймаут-класс ошибки: embed-вызов не уложился (сервер жив, но медлен).
fn is_embed_timeout(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<super::ollama::EmbedError>(),
        Some(super::ollama::EmbedError::Timeout { .. })
    )
}

/// Эмбедд батча + атомарная запись. Векторы приходят уже нормированными
/// из клиента; размерность канона — из meta (свежий индекс adopt'ит
/// фактическую dim первого ответа).
async fn embed_and_upsert(
    ollama: &OllamaClient,
    store: &mut Store,
    rows: &[KernelRow],
    timeout: Duration,
    report: &mut SyncReport,
) -> Result<()> {
    let index_dim = stored_dim(store);
    let inputs: Vec<String> = rows.iter().map(|r| r.content.clone()).collect();
    let vectors = ollama.embed(&inputs, index_dim, timeout).await?;
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
    let tx = store.begin()?;
    Store::upsert_batch(&tx, &records, None)?;
    tx.commit()?;
    report.embedded += records.len();
    Ok(())
}
