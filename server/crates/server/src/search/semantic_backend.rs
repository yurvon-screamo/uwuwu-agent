//! SemanticBackend на LanceDB embedded (решение задачи #1133): таблица чанков
//! (slug/idx/text/vector) + FTS (BM25) — гибрид с векторным поиском.
//! Эмбеддинги — Ollama-совместимый endpoint (локальная машина через туннель).
//! Инкремент — doc_hash в мета-таблице.

use crate::search::semantic::{chunk_text, OllamaEmbeddings, EMBED_MODEL_DIMS};
use async_trait::async_trait;
use lancedb::arrow::arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Int32Array, Int64Array, LargeStringArray,
    RecordBatch,
};
use lancedb::arrow::arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema};
use lancedb::connection::ConnectBuilder;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::Table;
use lancedb::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use uwuwu_usecase::SemanticBackend;
use uwuwu_usecase::WikiRepo;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

const CHUNKS: &str = "chunks";
const DOCS: &str = "doc_meta";

fn err(msg: String) -> UsecaseError {
    UsecaseError::State(format!("lance: {msg}"))
}

pub struct LanceSemanticBackend {
    wiki: Arc<dyn WikiRepo>,
    /// Персист sha-инкремента: /…/semantic_shas.json рядом с Lance-том.
    shas_file: PathBuf,
    /// Задачи для индекса (юзер-стори «по вики/задачам/опыту»); None — стенд.
    issues: Option<Arc<dyn uwuwu_usecase::IssueIndexSource>>,
    db: Connection,
    embeddings: Arc<OllamaEmbeddings>,
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
        embeddings: Arc<OllamaEmbeddings>,
    ) -> UsecaseResult<Self> {
        let db = ConnectBuilder::new(path.to_str().unwrap_or_default())
            .execute()
            .await
            .map_err(|e| err(format!("connect: {e}")))?;
        let shas_file = path.join("semantic_shas.json");
        let this = Self {
            wiki,
            shas_file,
            issues,
            db,
            embeddings,
        };
        this.ensure_tables().await?;
        Ok(this)
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
        self.db
            .open_table(CHUNKS)
            .execute()
            .await
            .map_err(|e| err(format!("open chunks: {e}")))
    }

    async fn docs(&self) -> UsecaseResult<Table> {
        self.db
            .open_table(DOCS)
            .execute()
            .await
            .map_err(|e| err(format!("open docs: {e}")))
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

    async fn reindex_doc(&self, slug: &str, md: &str, h: i64) -> UsecaseResult<usize> {
        self.chunks()
            .await?
            .delete(format!("slug = '{slug}'").as_str())
            .await
            .map_err(|e| err(format!("del chunks {slug}: {e}")))?;
        self.docs()
            .await?
            .delete(format!("slug = '{slug}'").as_str())
            .await
            .map_err(|e| err(format!("del meta {slug}: {e}")))?;
        let texts = chunk_text(md);
        if texts.is_empty() {
            return Ok(0);
        }
        let vectors = self.embeddings.embed_batch(&texts).await?;
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
        Ok(1)
    }
}

#[async_trait]
impl SemanticBackend for LanceSemanticBackend {
    async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<(f32, String)>> {
        let vectors = self.embeddings.embed_batch(&[query.to_string()]).await?;
        let Some(qv) = vectors.first() else {
            return Err(err("embedding запроса пуст".into()));
        };
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
                    // FTS-индекса может не быть до первого sync — честная пустота
                    tracing::debug!(event = "semantic.fts_unavailable", error = %e, "FTS недоступен");
                    return Ok(Vec::new());
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
                    self.reindex_doc(slug, &md, h).await?;
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
                        self.reindex_doc(&slug, &text, h).await?;
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
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
