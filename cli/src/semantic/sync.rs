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
//!   переэмбеддинга. Вынесен из поискового пути (задача 20261112203959:
//!   repair-эмбедды длинных батчей структурно не помещаются в 10-секундный
//!   остаток поиска, хронный «reconcile не прошёл» + ~550 SQL-запросов
//!   скана на каждый поиск) — reconcile живёт в `semantic sync`.

use crate::siyuan::SiYuan;
use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

use super::ollama::OllamaClient;
use super::store::{ChunkMeta, ChunkRecord, IndexedRow, Store};

/// Целевые типы блоков-чанков (контентные; l/d/av/tb — не чанки).
pub(crate) const TARGET_TYPES_SQL: &str = "('p','i','h','c','t')";
/// Страница kernel-выборки (явный LIMIT каждого stmt).
pub(crate) const PAGE: usize = 256;
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
/// Верхняя граница чанка (символы): html_embed-стены и крупные код-блоки
/// за ней дают эмбеддинг «головы» (ctx Ollama ~8192 токенов) — не индексируем.
/// Заземлено живым замером корпуса 06.11: выше границы 6 блоков из ~74k.
pub(crate) const MAX_CONTENT_CHARS: usize = 8000;
/// Overlap окна дельты: правки в ту же секунду, что watermark.
pub(crate) const OVERLAP_SECS: i64 = 60;

pub struct SyncOptions {
    /// None → от watermark из meta минус overlap; Some(s) → стартовая граница
    /// (rebuild: Some("0") — с нуля).
    pub since: Option<String>,
    pub deadline: Option<Instant>,
    /// Потолок таймаута одного embed-вызова (rebuild — 120s, поиск —
    /// SEARCH_EMBED_TIMEOUT); фактический таймаут = min(cap, остаток deadline).
    pub embed_timeout: Duration,
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
    /// Дрейф путей, освежённый без переэмбедда (hpath/box/root при
    /// неизменных updated и content).
    pub paths_refreshed: usize,
    /// Бюджет исчерпан до полной дотяжки — хвост останется над watermark.
    pub budget_exhausted: bool,
}

#[derive(Clone, Debug)]
pub struct KernelRow {
    pub meta: ChunkMeta,
    pub content: String,
}

pub(crate) fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Окна эмбеддинга: слайсы набора строк по EMBED_BATCH строк И по
/// EMBED_BATCH_CHARS суммарных символов — набор длинных чанков не собирается
/// в батч, чей embed дольше таймаута пути; watermark двигается каждым
/// записанным окном, дельта проходит сквозь страницы длинных доков.
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

/// Таймаут embed-вызова: потолок синка минус остаток wall-clock бюджета —
/// каждый вызов укладывается в общий контракт поиска (п.2 ревью PR #78),
/// а не получает свежий полный таймаут поверх израсходованного бюджета.
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

/// Watermark минус overlap; некорректный формат — ошибка (мусор в meta).
fn since_with_overlap(watermark: &str) -> Result<String> {
    let ts = chrono::NaiveDateTime::parse_from_str(watermark, "%Y%m%d%H%M%S")
        .map_err(|e| anyhow::anyhow!("watermark «{watermark}» не парсится: {e}"))?;
    Ok((ts - chrono::Duration::seconds(OVERLAP_SECS))
        .format("%Y%m%d%H%M%S")
        .to_string())
}

/// Результат классификации страницы дельты (чистый, без IO).
struct PageOutcome {
    /// Строки полосы [MIN, MAX] — кандидаты на эмбедд.
    pending: Vec<KernelRow>,
    /// Усохшие/переросшие строки — prune-кандидаты (DELETE отсутствующих
    /// no-op по PK; вернуть блок может правка).
    prune_ids: Vec<String>,
    /// Дрейф метаданных (hpath/box/root) при неизменных (updated, content) —
    /// освежается без переэмбедда.
    drift: Vec<ChunkMeta>,
    cursor_updated: String,
    cursor_id: String,
    skipped_short: usize,
    skipped_long: usize,
}

/// Классификация страницы дельты: курсор двигается по всем найденным
/// строкам (включая пропущенные по длине), исчезнувшие между сканом и
/// fetch'ем курсор не двигают — их поймает reconcile. Строка, уже
/// проиндексированная с теми же (updated, content), — чистый skip:
/// overlap-перескан не переэмбеддит идемпотентное (анти-карусель
/// 20261106182055), а расхождение только в путях — дрейф без эмбедда.
fn process_page(
    rows: &[Value],
    by_id: &std::collections::HashMap<String, KernelRow>,
    indexed: &std::collections::HashMap<String, IndexedRow>,
) -> PageOutcome {
    let mut outcome = PageOutcome {
        pending: Vec::with_capacity(rows.len()),
        prune_ids: Vec::new(),
        drift: Vec::new(),
        cursor_updated: String::new(),
        cursor_id: String::new(),
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
        outcome.cursor_updated = kernel_row.meta.updated.clone();
        outcome.cursor_id = kernel_row.meta.block_id.clone();
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

/// Стартовый курсор дельты: явный since (rebuild) или watermark минус
/// overlap; пустой watermark — с нуля; мусор в meta — громкая ошибка.
/// Overlap сдвигает только секунду: id-компонента пары на resume не
/// сужает перескан (тот же at-least-once, что у голого watermark).
fn starting_cursor(store: &Store, since: &Option<String>) -> Result<(String, String)> {
    let updated = match since {
        Some(since) => since.clone(),
        None => match store.meta("watermark")? {
            Some(mark) => {
                let (watermark_updated, _) = split_watermark(&mark);
                since_with_overlap(&watermark_updated)?
            }
            None => "0".to_string(),
        },
    };
    Ok((updated, String::new()))
}

/// Композитный курсор «updated|id»: лексикография = порядок (updated, id)
/// (updated фиксированной ширины 14 цифр; id сравнивается строкой — тот же
/// порядок, что keyset ядра). Сепаратор '|' в обеих частях не встречается.
pub(crate) fn watermark_composite(updated: &str, id: &str) -> String {
    format!("{updated}|{id}")
}

/// Обратный разбор композита; legacy-значение без '|' (водяной знак
/// до pair-фикса) — секунда известна, id-курсор пуст.
pub(crate) fn split_watermark(mark: &str) -> (String, String) {
    match mark.split_once('|') {
        Some((updated, id)) => (updated.to_string(), id.to_string()),
        None => (mark.to_string(), String::new()),
    }
}

/// Шаг scan-page: страница дельты + точечный fetch полных строк по PK,
/// индексированные по id — тяжёлый content не идёт через keyset-скан
/// (ядро без индекса по updated, полный скан таскает мегабайты).
/// Возвращает и ids страницы: они нужны delta_sync для indexed_rows.
async fn fetch_classifiable_page(
    sy: &SiYuan,
    cursor_updated: &str,
    cursor_id: &str,
) -> Result<(
    Vec<Value>,
    std::collections::HashMap<String, KernelRow>,
    Vec<String>,
)> {
    let rows = sy.sql(&delta_page_stmt(cursor_updated, cursor_id)).await?;
    if rows.is_empty() {
        return Ok((rows, std::collections::HashMap::new(), Vec::new()));
    }
    let page_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    let full_rows = fetch_by_ids(sy, &page_ids).await?;
    let by_id = full_rows
        .into_iter()
        .map(|row| (row.meta.block_id.clone(), row))
        .collect();
    Ok((rows, by_id, page_ids))
}

/// Дельта-цикл: страницы → skip идемпотентного / фильтр длины → эмбедд
/// батчами → upsert+watermark-пара.
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
    let (mut cursor_updated, mut cursor_id) = starting_cursor(store, &opts.since)?;

    loop {
        if !budget_left(opts.deadline) {
            report.budget_exhausted = true;
            break;
        }
        let (rows, by_id, page_ids) =
            fetch_classifiable_page(sy, &cursor_updated, &cursor_id).await?;
        if rows.is_empty() {
            break;
        }
        let page_len = rows.len();
        let indexed = store.indexed_rows(&page_ids)?;

        let outcome = process_page(&rows, &by_id, &indexed);
        cursor_updated = outcome.cursor_updated;
        cursor_id = outcome.cursor_id;
        report.skipped_short += outcome.skipped_short;
        report.skipped_long += outcome.skipped_long;

        // Усохшие/переросшие чанки уходят из индекса в том же прогоне —
        // иначе дельта, пропустив строку, оставляла бы в индексе вечно
        // устаревший контент с вектором (п.1 ревью PR #78).
        if !outcome.prune_ids.is_empty() {
            report.pruned += store.prune(&outcome.prune_ids)?;
        }
        // Дрейф путей живёт без переэмбедда: сменился hpath/box/root при
        // неизменных (updated, content) — метаданные в том же прогоне.
        if !outcome.drift.is_empty() {
            report.paths_refreshed += store.refresh_paths(&outcome.drift)?;
        }

        if !embed_batches(
            ollama,
            store,
            &embed_batch_windows(&outcome.pending),
            opts,
            &mut report,
        )
        .await?
        {
            break;
        }
        if page_len < PAGE {
            // Чистый конец: весь просмотренный диапазон обработан — watermark
            // на крайний курсор-пару (включая пропущенные по длине строки);
            // запись — с MAX-гвардом (только вперёд).
            store.advance_watermark(&watermark_composite(&cursor_updated, &cursor_id))?;
            break;
        }
        if let Some(delay) = opts.page_delay {
            tokio::time::sleep(delay).await;
        }
    }
    Ok(report)
}

/// Эмбедд батчей с бюджет-гейтом на каждый батч; false — бюджет исчерпан
/// (хвост остаётся над watermark и дотягивается следующими запусками).
async fn embed_batches(
    ollama: &OllamaClient,
    store: &mut Store,
    batches: &[&[KernelRow]],
    opts: &SyncOptions,
    report: &mut SyncReport,
) -> Result<bool> {
    for batch in batches {
        if !budget_left(opts.deadline) {
            report.budget_exhausted = true;
            return Ok(false);
        }
        embed_and_upsert(
            ollama,
            store,
            batch,
            embed_call_timeout(opts.deadline, opts.embed_timeout),
            report,
        )
        .await?;
        if let Some(on_batch) = &opts.on_batch {
            on_batch(report.embedded);
        }
    }
    Ok(true)
}

/// Эмбедд батча + атомарная запись (чанки и watermark одной транзакцией).
/// Векторы приходят уже нормированными из клиента; размерность канона —
/// из meta (свежий индекс adopt'ит фактическую dim первого ответа).
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
    let watermark = rows
        .last()
        .map(|r| watermark_composite(&r.meta.updated, &r.meta.block_id));
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
}

/// Полный id-diff ядра и индекса: самоисцеление пропусков (backfill),
/// prune исчезнувших (в т.ч. трэш — блоки удалённых доков выпадают из
/// kernel SQL), дрейф путей без переэмбеддинга. last_reconcile_at
/// ставится только по завершённому прогону — оборванный бюджет
/// повторится при следующем sync. Вызывается только из `semantic sync`:
/// repair-эмбедды ходят с REBUILD-классом таймаута (embed_timeout).
pub async fn reconcile(
    sy: &SiYuan,
    ollama: &OllamaClient,
    store: &mut Store,
    deadline: Option<Instant>,
    embed_timeout: Duration,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let now = chrono::Utc::now().timestamp();

    let kernel = scan_kernel_pages(sy, deadline).await?;
    if !kernel.completed {
        // Прерванный бюджетом скан — рутинное событие sync-пути
        // (бюджет 600 s, живой бэклог). Частичный kernel против индекса
        // дал бы ложные vanished — весь несосканированный хвост был бы
        // вычищен (High ревью PR #96). Прерванный прогон — no-op: без
        // prune, без last_reconcile_at; повторится при следующем sync.
        return Ok(report);
    }
    let diff = diff_index(&kernel.metas, &store.chunk_metas()?);

    if !diff.vanished.is_empty() {
        report.pruned = store.prune(&diff.vanished)?;
    }
    if !diff.path_drift.is_empty() {
        report.paths_refreshed = store.refresh_paths(&diff.path_drift)?;
    }

    let repaired =
        repair_missing(sy, ollama, store, &diff.repair_ids, deadline, embed_timeout).await?;
    if !repaired.completed {
        // Частичный backfill не фиксирует прогон — дыры ретраятся
        // следующим поиском, а не через сутки (Common ревью PR #96).
        return Ok(report);
    }
    report.pruned += repaired.pruned;
    report.repaired += repaired.repaired;

    // last_reconcile_at только по завершённому прогону — оборванный
    // бюджетом повторится на следующем поиске.
    store.set_meta("last_reconcile_at", &now.to_string())?;
    Ok(report)
}

/// Результат скана ядра: меты всех целевых блоков + признак завершённости.
struct ScanOutcome {
    metas: Vec<ChunkMeta>,
    /// Скан дошёл до конца ядра; false = бюджет исчерпан посреди —
    /// прогон не засчитывается (см. reconcile).
    completed: bool,
}

/// Лёгкий полный скан ядра: keyset по id, без content.
async fn scan_kernel_pages(sy: &SiYuan, deadline: Option<Instant>) -> Result<ScanOutcome> {
    let mut kernel: Vec<ChunkMeta> = Vec::new();
    let mut cursor = String::new();
    loop {
        if !budget_left(deadline) {
            return Ok(ScanOutcome {
                metas: kernel,
                completed: false,
            });
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
            return Ok(ScanOutcome {
                metas: kernel,
                completed: true,
            });
        }
    }
}

/// Три класса расхождений ядра и индекса; порядок проверок важен:
/// изменившийся updated ремонтируется полным переэмбеддом (заберёт
/// и свежий hpath).
struct IndexDiff {
    /// В ядре есть, в индексе нет (или updated разошёлся) — repair.
    repair_ids: Vec<String>,
    /// Дрейф путей — метаданные без переэмбеддинга.
    path_drift: Vec<ChunkMeta>,
    /// В индексе есть, в ядре нет (удаление/трэш) — prune.
    vanished: Vec<String>,
}

fn diff_index(kernel: &[ChunkMeta], index: &[ChunkMeta]) -> IndexDiff {
    let index_by_id: std::collections::HashMap<&str, &ChunkMeta> =
        index.iter().map(|m| (m.block_id.as_str(), m)).collect();
    let kernel_by_id: std::collections::HashMap<&str, &ChunkMeta> =
        kernel.iter().map(|m| (m.block_id.as_str(), m)).collect();

    let mut diff = IndexDiff {
        repair_ids: Vec::new(),
        path_drift: Vec::new(),
        vanished: Vec::new(),
    };
    for meta in kernel {
        match index_by_id.get(meta.block_id.as_str()) {
            None => diff.repair_ids.push(meta.block_id.clone()),
            Some(indexed) if indexed.updated != meta.updated => {
                diff.repair_ids.push(meta.block_id.clone())
            }
            Some(indexed)
                if indexed.hpath != meta.hpath
                    || indexed.box_id != meta.box_id
                    || indexed.root_id != meta.root_id =>
            {
                diff.path_drift.push(meta.clone())
            }
            _ => {}
        }
    }
    diff.vanished = index_by_id
        .keys()
        .filter(|id| !kernel_by_id.contains_key(*id))
        .map(|id| id.to_string())
        .collect();
    diff
}

/// Счётчики repair-прохода + признак завершённости.
#[derive(Debug)]
pub(crate) struct RepairOutcome {
    pub(crate) repaired: usize,
    pub(crate) pruned: usize,
    /// Repair дошёл до конца списка; false = бюджет исчерпан посреди —
    /// прогон не засчитывается (см. reconcile).
    pub(crate) completed: bool,
}

/// Backfill/переэмбедд недостающих строк: классификация по длине —
/// усохшие/переросшие блоки вычищаются из индекса (а при backfill просто
/// не индексируются), полосу [MIN, MAX] ремонтирует переэмбедд.
/// Таймаут эмбедда прокидывается вызывающим (sync — REBUILD-класс).
/// Тестовый вход закрепляет контракт частичного прогона.
pub(crate) async fn repair_missing(
    sy: &SiYuan,
    ollama: &OllamaClient,
    store: &mut Store,
    repair_ids: &[String],
    deadline: Option<Instant>,
    embed_timeout: Duration,
) -> Result<RepairOutcome> {
    let mut outcome = RepairOutcome {
        repaired: 0,
        pruned: 0,
        completed: true,
    };
    for id_batch in repair_ids.chunks(PAGE) {
        if !budget_left(deadline) {
            outcome.completed = false;
            return Ok(outcome);
        }
        let rows = fetch_by_ids(sy, id_batch).await?;
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
            outcome.pruned += store.prune(&prune_ids)?;
        }
        for batch in embed_batch_windows(&target) {
            if !budget_left(deadline) {
                outcome.completed = false;
                return Ok(outcome);
            }
            let mut delta_report = SyncReport::default();
            embed_and_upsert(
                ollama,
                store,
                batch,
                embed_call_timeout(deadline, embed_timeout),
                &mut delta_report,
            )
            .await?;
            outcome.repaired += delta_report.embedded;
        }
    }
    Ok(outcome)
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
