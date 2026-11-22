//! SemanticBackend на LanceDB embedded (решение задачи #1133): таблица чанков
//! (slug/idx/text/vector) + FTS (BM25) — гибрид с векторным поиском.
//! Эмбеддинги — Gemini Embedding 2 через Embedder-трейт (#1363).
//! Инкремент — doc_hash в мета-таблице.

use crate::search::semantic::{chunk_text, Embedder, EMBED_BATCH, EMBED_MODEL_DIMS};
use async_trait::async_trait;
use lancedb::arrow::arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Int32Array, Int64Array, LargeStringArray,
    RecordBatch,
};
use lancedb::arrow::arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema};
use lancedb::connection::ConnectBuilder;
use lancedb::index::scalar::{FtsIndexBuilder, FullTextSearchQuery};
use lancedb::index::{Index, IndexType};
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::Table;
use lancedb::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uwuwu_usecase::SemanticBackend;
use uwuwu_usecase::WikiRepo;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

const CHUNKS: &str = "chunks";
const DOCS: &str = "doc_meta";

pub(super) fn err(msg: String) -> UsecaseError {
    UsecaseError::State(format!("lance: {msg}"))
}

/// SQL-литерал предиката по слагу: апостроф удваивается по SQL-конвенции
/// DataFusion (`''`) — сырая интерполяция рвёт литерал на слагах с `'`
/// (#1392: Unterminated string literal → sync в часовом бэкоффе).
fn slug_pred(slug: &str) -> String {
    format!("slug = '{}'", slug.replace('\'', "''"))
}

pub struct LanceSemanticBackend {
    wiki: Arc<dyn WikiRepo>,
    /// Персист sha-инкремента: /…/semantic_shas.json рядом с Lance-том.
    shas_file: PathBuf,
    /// Суточный бюджет эмбеддингов синка (#1473): /…/semantic_budget.json
    /// рядом с Lance-том — резерв дневной квоты Gemini для query-поисков.
    budget_file: PathBuf,
    /// Задачи для индекса (юзер-стори «по вики/задачам/опыту»); None — стенд.
    issues: Option<Arc<dyn uwuwu_usecase::IssueIndexSource>>,
    db: Connection,
    embeddings: Arc<dyn Embedder>,
}

/// Размерность векторной колонки живой таблицы (FixedSizeList list_size).
fn vector_dim(schema: &ArrowSchema) -> Option<usize> {
    match schema.field_with_name("vector").ok()?.data_type() {
        DataType::FixedSizeList(_, size) => Some(*size as usize),
        _ => None,
    }
}

/// Dim-guard (#1363): dims живой таблицы ≠ размерности активного клиента —
/// пространства эмбеддингов несовместимы, индекс «тихо смешался» бы.
/// True — таблицы подлежат сносу (переиндексирует штатный sync).
fn dims_stale(dim: Option<usize>) -> bool {
    match dim {
        Some(d) => d != EMBED_MODEL_DIMS,
        None => false, // нет таблицы/колонки — гарду делать нечего
    }
}

fn chunks_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        ArrowField::new("slug", DataType::LargeUtf8, false),
        ArrowField::new("idx", DataType::Int32, false),
        ArrowField::new("text", DataType::LargeUtf8, false),
        ArrowField::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(ArrowField::new("item", DataType::Float32, true)),
                EMBED_MODEL_DIMS as i32,
            ),
            false,
        ),
    ]))
}

fn docs_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        ArrowField::new("slug", DataType::LargeUtf8, false),
        ArrowField::new("doc_hash", DataType::Int64, false),
    ]))
}

impl LanceSemanticBackend {
    pub async fn open(
        path: PathBuf,
        wiki: Arc<dyn WikiRepo>,
        issues: Option<Arc<dyn uwuwu_usecase::IssueIndexSource>>,
        embeddings: Arc<dyn Embedder>,
    ) -> UsecaseResult<Self> {
        let db = ConnectBuilder::new(path.to_str().unwrap_or_default())
            .execute()
            .await
            .map_err(|e| err(format!("connect: {e}")))?;
        // Dim-guard (#1363): живые таблицы с чужой размерностью сносятся —
        // пространства эмбеддингов несовместимы, штатный sync перестроит индекс
        // из первоисточника. Прозрачная переиндексация при смене модели/размерности.
        let names = db
            .table_names()
            .execute()
            .await
            .map_err(|e| err(format!("list: {e}")))?;
        if names.iter().any(|n| n == CHUNKS) {
            let table = db
                .open_table(CHUNKS)
                .execute()
                .await
                .map_err(|e| err(format!("open chunks: {e}")))?;
            // схема недоступна → dim None → guard no-op (ошибка вылезет ниже
            // на ensure/чтении — здесь она не должна ломать миграционный путь)
            let dim = table.schema().await.ok().and_then(|s| vector_dim(&s));
            if dims_stale(dim) {
                tracing::warn!(
                    event = "semantic.dim_migration",
                    old_dim = ?dim, new_dim = EMBED_MODEL_DIMS,
                    "vector dims changed — индекс снесён, sync перестроит из первоисточника"
                );
                db.drop_table(CHUNKS, &[])
                    .await
                    .map_err(|e| err(format!("drop chunks: {e}")))?;
                db.drop_table(DOCS, &[])
                    .await
                    .map_err(|e| err(format!("drop meta: {e}")))?;
                let _ = std::fs::remove_file(path.join("semantic_shas.json"));
            }
        }
        let shas_file = path.join("semantic_shas.json");
        let this = Self {
            wiki,
            shas_file,
            budget_file: super::budget::budget_path(&path),
            issues,
            db,
            embeddings,
        };
        this.ensure_tables().await?;
        this.ensure_fts_index().await;
        Ok(this)
    }

    /// FTS-индекс на тексте чанков: без него полнотекст молча пуст —
    /// ланс не даёт брутфорс-план на колонке без инвертированного индекса
    /// (корень «мёртвого полнотекста» #1473, квото-независимый). Создаётся
    /// один раз при открытии; дописанные после индексации строки покрывает
    /// flat-путь ланса. Отказ создания не роняет открытие: семантика и
    /// sync живут, полнотекст честно пуст до следующего бута (warn).
    async fn ensure_fts_index(&self) {
        let table = match self.chunks().await {
            Ok(table) => table,
            Err(e) => {
                tracing::warn!(
                    event = "semantic.fts_index_skip",
                    error = %e,
                    "FTS-индекс не проверен: таблица чанков недоступна"
                );
                return;
            }
        };
        let has_fts = match table.list_indices().await {
            Ok(indices) => indices
                .iter()
                .any(|i| matches!(i.index_type, IndexType::FTS) && i.columns.iter().any(|c| c == "text")),
            Err(e) => {
                tracing::warn!(
                    event = "semantic.fts_index_skip",
                    error = %e,
                    "FTS-индекс не проверен: список индексов недоступен"
                );
                return;
            }
        };
        if has_fts {
            return;
        }
        if let Err(e) = table
            .create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
            .execute()
            .await
        {
            tracing::warn!(
                event = "semantic.fts_index_failed",
                error = %e,
                "FTS-индекс не создан — полнотекст пуст до следующего бута"
            );
            return;
        }
        tracing::info!(
            event = "semantic.fts_index_created",
            "FTS-индекс создан — полнотекст жив"
        );
    }

    async fn ensure_tables(&self) -> UsecaseResult<()> {
        for (name, schema) in [(CHUNKS, chunks_schema()), (DOCS, docs_schema())] {
            let names = self
                .db
                .table_names()
                .execute()
                .await
                .map_err(|e| err(format!("list: {e}")))?;
            if !names.iter().any(|n| n == name) {
                self.db
                    .create_empty_table(name, schema)
                    .execute()
                    .await
                    .map_err(|e| err(format!("create {name}: {e}")))?;
            }
        }
        Ok(())
    }

    async fn chunks(&self) -> UsecaseResult<Table> {
        self.table_named(CHUNKS).await
    }

    async fn docs(&self) -> UsecaseResult<Table> {
        self.table_named(DOCS).await
    }

    /// Обслуживание таблиц после чурна (#1436): компакция мелких фрагментов
    /// и prune версий/файлов по ретеншену. Ретеншен — параметр: прод-вызов
    /// из sync_index — константа `PROD_RETENTION`, интеграционный пин —
    /// ноль (чурн-петля теста живёт секунды).
    pub async fn maintain(&self, older_than: Duration) -> UsecaseResult<()> {
        for (name, label) in [(CHUNKS, "chunks"), (DOCS, "meta")] {
            let table = self.table_named(name).await?;
            crate::search::maintenance::maintain_table(table, label, older_than).await?;
        }
        Ok(())
    }

    async fn table_named(&self, name: &str) -> UsecaseResult<Table> {
        self.db
            .open_table(name)
            .execute()
            .await
            .map_err(|e| err(format!("open {name}: {e}")))
    }

    /// slug → git blob-sha последнего проиндексированного док-слага
    /// (мелкий JSON-персист рядом с Lance; sha-инкремент синка).
    fn known_shas(&self) -> HashMap<String, String> {
        std::fs::read_to_string(self.shas_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn mark_known_sha(&self, slug: &str, sha: &str) {
        let mut m = self.known_shas();
        m.insert(slug.to_string(), sha.to_string());
        let _ = std::fs::write(
            self.shas_path(),
            serde_json::to_string(&m).unwrap_or_default(),
        );
    }

    fn shas_path(&self) -> PathBuf {
        self.shas_file.clone()
    }

    async fn known_hashes(&self) -> UsecaseResult<HashMap<String, i64>> {
        let stream = self
            .docs()
            .await?
            .query()
            .execute()
            .await
            .map_err(|e| err(format!("docs read: {e}")))?;
        use futures::stream::StreamExt;
        let mut out = HashMap::new();
        let mut stream = stream;
        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(|e| err(format!("docs stream: {e}")))?;
            let slugs = batch
                .column_by_name("slug")
                .and_then(|c| c.as_any().downcast_ref::<LargeStringArray>());
            let hashes = batch
                .column_by_name("doc_hash")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            if let (Some(s), Some(h)) = (slugs, hashes) {
                for i in 0..s.len() {
                    out.insert(s.value(i).to_string(), h.value(i));
                }
            }
        }
        Ok(out)
    }

    fn hash(s: &str) -> i64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        (h & 0x7fff_ffff_ffff_ffff) as i64
    }

    /// Cost реиндекса в запросах к Gemini: тексты режутся батчами по
    /// EMBED_BATCH в embed_many — ceil(чанков/батч) точно повторяет разбиение.
    fn embed_request_cost(chunks: usize) -> u64 {
        chunks.div_ceil(EMBED_BATCH) as u64
    }

    /// Гард суточного бюджета (L1 #1473): cost учитывается ДО embed-вызова
    /// без отката при 429 — консервативный перебор безопаснее недобора
    /// (резерв query-поисков неприкосновенен). False — кап достигнут, синк
    /// останавливается; отказ персиста не роняет индексацию (недоучёт
    /// деградирует мягко через честный 429).
    fn daily_budget_admits(&self, spend: &mut super::budget::BudgetState, chunks: usize) -> bool {
        let cost = Self::embed_request_cost(chunks);
        if spend.spent + cost > super::budget::SYNC_DAILY_CAP {
            tracing::warn!(
                event = "semantic.daily_budget_exhausted",
                spent = spend.spent,
                cap = super::budget::SYNC_DAILY_CAP,
                "Суточный бюджет эмбеддингов синка исчерпан — до резета (полночь PT) поиски идут на резерве"
            );
            return false;
        }
        spend.spent += cost;
        if let Err(e) = super::budget::write_state(&self.budget_file, spend) {
            tracing::warn!(
                event = "semantic.budget_persist_failed",
                error = %e,
                "Счётчик бюджета не записан — недоучёт расхода до рестарта"
            );
        }
        true
    }

    async fn reindex_doc_chunks(
        &self,
        slug: &str,
        texts: Vec<String>,
        h: i64,
    ) -> UsecaseResult<()> {
        self.chunks()
            .await?
            .delete(slug_pred(slug).as_str())
            .await
            .map_err(|e| err(format!("del chunks {slug}: {e}")))?;
        self.docs()
            .await?
            .delete(slug_pred(slug).as_str())
            .await
            .map_err(|e| err(format!("del meta {slug}: {e}")))?;
        if texts.is_empty() {
            return Ok(());
        }
        // doc-пары (title, text): retrieval-контракт клиента (#1363)
        let docs: Vec<(String, String)> = texts
            .iter()
            .map(|t| (slug.to_string(), t.clone()))
            .collect();
        let vectors = self.embeddings.embed_docs(&docs).await?;
        let slugs: Vec<String> = vec![slug.to_string(); texts.len()];
        let idxs: Vec<i32> = (0..texts.len() as i32).collect();
        let flat: Vec<f32> = vectors.iter().flat_map(|v| v.iter().copied()).collect();
        let values = Float32Array::from(flat);
        let vector_col = FixedSizeListArray::new(
            Arc::new(ArrowField::new("item", DataType::Float32, true)),
            EMBED_MODEL_DIMS as i32,
            Arc::new(values) as ArrayRef,
            None,
        );
        let batch = RecordBatch::try_new(
            chunks_schema(),
            vec![
                Arc::new(LargeStringArray::from(slugs)),
                Arc::new(Int32Array::from(idxs)),
                Arc::new(LargeStringArray::from(texts)),
                Arc::new(vector_col),
            ],
        )
        .map_err(|e| err(format!("batch: {e}")))?;
        self.chunks()
            .await?
            .add(batch)
            .execute()
            .await
            .map_err(|e| err(format!("add chunks {slug}: {e}")))?;
        let meta_batch = RecordBatch::try_new(
            docs_schema(),
            vec![
                Arc::new(LargeStringArray::from(vec![slug.to_string()])),
                Arc::new(Int64Array::from(vec![h])),
            ],
        )
        .map_err(|e| err(format!("meta batch: {e}")))?;
        self.docs()
            .await?
            .add(meta_batch)
            .execute()
            .await
            .map_err(|e| err(format!("add meta {slug}: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl SemanticBackend for LanceSemanticBackend {
    async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<(f32, String)>> {
        let qv = self.embeddings.embed_query(query).await?;
        let vq = self
            .chunks()
            .await?
            .vector_search(qv.clone())
            .map_err(|e| err(format!("vector build: {e}")))?;
        let stream = vq
            .limit(top * 3)
            .execute()
            .await
            .map_err(|e| err(format!("vector: {e}")))?;
        use futures::stream::StreamExt;
        let mut stream = stream;
        let mut best: HashMap<String, f32> = HashMap::new();
        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(|e| err(format!("vstream: {e}")))?;
            let slugs = batch
                .column_by_name("slug")
                .and_then(|c| c.as_any().downcast_ref::<LargeStringArray>());
            let dists = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            if let (Some(s), Some(d)) = (slugs, dists) {
                for i in 0..s.len() {
                    let similarity = 1.0 - d.value(i);
                    let e = best.entry(s.value(i).to_string()).or_insert(f32::MIN);
                    if similarity > *e {
                        *e = similarity;
                    }
                }
            }
        }
        let mut out: Vec<(f32, String)> = best.into_iter().map(|(k, v)| (v, k)).collect();
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(top);
        Ok(out)
    }

    async fn fulltext(&self, tokens: &[String], top: usize) -> UsecaseResult<Vec<String>> {
        let table = self.chunks().await?;
        let mut per_token: Vec<std::collections::HashSet<String>> = Vec::new();
        for token in tokens {
            let q = FullTextSearchQuery::new(token.to_string());
            let stream = match table
                .query()
                .full_text_search(q)
                .limit(top * 3)
                .execute()
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    // #1473: отказ FTS не молчит — Err пробрасывается в
                    // usecase (рендер «— недоступен»), причина видна в выдаче
                    tracing::warn!(event = "semantic.fts_unavailable", error = %e, "FTS недоступен");
                    return Err(err(format!("fts: {e}")));
                }
            };
            use futures::stream::StreamExt;
            let mut stream = stream;
            let mut hits = std::collections::HashSet::new();
            while let Some(batch) = stream.next().await {
                let batch = batch.map_err(|e| err(format!("fts stream: {e}")))?;
                if let Some(col) = batch.column_by_name("slug") {
                    if let Some(arr) = col.as_any().downcast_ref::<LargeStringArray>() {
                        for i in 0..arr.len() {
                            hits.insert(arr.value(i).to_string());
                        }
                    }
                }
            }
            per_token.push(hits);
        }
        // AND-семантика токенов
        let mut it = per_token.into_iter();
        let Some(first) = it.next() else {
            return Ok(Vec::new());
        };
        let mut inter: Vec<String> = first.into_iter().collect();
        for set in it {
            inter.retain(|s| set.contains(s));
            if inter.is_empty() {
                break;
            }
        }
        inter.truncate(top);
        Ok(inter)
    }

    async fn sync_index(&self, budget: usize) -> UsecaseResult<usize> {
        // sha-инкремент: git blob-sha из дерева (один вызов) отсекает
        // неизменённые ДО чтения содержимого — иначе сотни read_doc
        // до первого изменённого = таймаут сайдкара (каскад 16.11)
        let shas: std::collections::HashMap<String, String> =
            self.wiki.doc_shas().await?.into_iter().collect();
        let known = self.known_hashes().await?;
        let known_shas = self.known_shas();
        // Суточный бюджет эмбеддингов (#1473): чужой/битый день — счёт с нуля.
        let mut spend = super::budget::read_state(&self.budget_file);
        let mut changed = 0usize;
        for (slug, sha) in &shas {
            if changed >= budget {
                break;
            }
            if known_shas.get(slug).map(String::as_str) == Some(sha.as_str()) {
                continue; // sha не шевелился — содержимое не читаем
            }
            if let Some(md) = self.wiki.read_doc(slug).await? {
                let h = Self::hash(&md);
                if known.get(slug).copied() != Some(h) {
                    let texts = chunk_text(&md);
                    if !self.daily_budget_admits(&mut spend, texts.len()) {
                        break;
                    }
                    self.reindex_doc_chunks(slug, texts, h).await?;
                    changed += 1;
                }
                self.mark_known_sha(slug, sha);
            }
        }
        // задачи в индекс (юзер-стори: «найти решённое раньше по вики/задачам»):
        // слаг task-N — выдача читается task_get по номеру
        if let Some(issues) = &self.issues {
            if changed < budget {
                for digest in issues.recent_issues(400).await? {
                    if changed >= budget {
                        break;
                    }
                    let slug = format!("task-{}", digest.number);
                    let text = format!(
                        "#{} {}\n\n{}",
                        digest.number,
                        digest.title,
                        digest.body.chars().take(4000).collect::<String>()
                    );
                    let h = Self::hash(&text);
                    if known.get(&slug).copied() != Some(h) {
                        let texts = chunk_text(&text);
                        if !self.daily_budget_admits(&mut spend, texts.len()) {
                            break;
                        }
                        self.reindex_doc_chunks(&slug, texts, h).await?;
                        changed += 1;
                    }
                }
            }
        }
        if changed > 0 {
            tracing::info!(
                event = "semantic.sync",
                docs = changed,
                "Lance index synced"
            );
            // Чурн этого синка плодит версии/фрагменты/deletion-файлы —
            // обслуживание сразу (#1436). Отказ гигиены не роняет синк:
            // индекс корректен, обслуживание догонит следующим чурном
            // (громкий warn вместо тишины и вместо часового бэкоффа синка).
            if let Err(e) = self
                .maintain(crate::search::maintenance::PROD_RETENTION)
                .await
            {
                tracing::warn!(
                    event = "semantic.maintain_failed",
                    error = %e,
                    "Lance maintenance failed — sync result intact"
                );
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancedb::arrow::arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema};
    use std::sync::Arc as StdArc;

    #[test]
    fn hash_stable() {
        assert_eq!(
            LanceSemanticBackend::hash("abc"),
            LanceSemanticBackend::hash("abc")
        );
        assert_ne!(
            LanceSemanticBackend::hash("abc"),
            LanceSemanticBackend::hash("abd")
        );
    }

    fn schema_with_vector(dim: i32) -> ArrowSchema {
        ArrowSchema::new(vec![ArrowField::new(
            "vector",
            DataType::FixedSizeList(
                StdArc::new(ArrowField::new("item", DataType::Float32, true)),
                dim,
            ),
            false,
        )])
    }

    #[test]
    fn vector_dim_reads_fixed_size_list() {
        assert_eq!(vector_dim(&schema_with_vector(1024)), Some(1024));
        assert_eq!(vector_dim(&schema_with_vector(1536)), Some(1536));
        let no_vector = ArrowSchema::new(vec![ArrowField::new("text", DataType::LargeUtf8, false)]);
        assert_eq!(vector_dim(&no_vector), None);
    }

    #[test]
    fn dim_guard_flags_only_mismatch() {
        assert!(dims_stale(Some(1024))); // qwen3-эпоха против Gemini 1536
        assert!(!dims_stale(Some(EMBED_MODEL_DIMS)));
        assert!(!dims_stale(None)); // пустой volume — миграции нет
    }
}
