//! Связка SemanticBackend: OpenRouter-эмбеддинги + плоский индекс.
//! Инкремент по doc_hash, чанки дока замещаются целиком (гигиена),
//! без ключа OpenRouter индекс строится текстом (полнотекст жив).

use crate::semantic::{chunk_text, text_hash, Chunk, FileSemanticStore, OpenRouterEmbeddings};
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::ports::WikiRepo;
use uwuwu_usecase::search::SemanticBackend;

pub struct FlatSemanticBackend {
    pub wiki: Arc<dyn WikiRepo>,
    pub store: Arc<FileSemanticStore>,
    /// None — текстовый индекс без векторов (полнотекст жив, семантика офлайн).
    pub embeddings: Option<Arc<OpenRouterEmbeddings>>,
}

#[async_trait]
impl SemanticBackend for FlatSemanticBackend {
    async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<(f32, String)>> {
        let Some(embeddings) = &self.embeddings else {
            return Err(UsecaseError::State(
                "семантика: нет ключа OpenRouter".into(),
            ));
        };
        let vecs = embeddings.embed_batch(&[query.to_string()]).await?;
        let Some(qv) = vecs.first() else {
            return Err(UsecaseError::State("embedding запроса пуст".into()));
        };
        let index = self.store.index.lock().expect("index lock");
        Ok(index.search(qv, top))
    }

    /// Полнотекст по текстам чанков (без сети).
    async fn fulltext(&self, tokens: &[String], top: usize) -> UsecaseResult<Vec<String>> {
        let index = self.store.index.lock().expect("index lock");
        Ok(index.fulltext(tokens, top))
    }

    async fn sync_index(&self, budget: usize) -> UsecaseResult<usize> {
        let all = self.wiki.list_docs("").await?;
        let mut changed: Vec<(String, String, u64)> = Vec::new();
        for slug in all {
            if changed.len() >= budget {
                break;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                let h = text_hash(&md);
                if self.store.doc_hash(&slug) != Some(h) {
                    changed.push((slug, md, h));
                }
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        // чанки (текст всегда; векторы — если ключ есть)
        let mut inputs: Vec<String> = Vec::new();
        for (_, md, _) in &changed {
            inputs.extend(chunk_text(md));
        }
        let vectors = match &self.embeddings {
            Some(e) => e.embed_batch(&inputs).await?,
            None => vec![Vec::new(); inputs.len()],
        };
        let mut it = vectors.into_iter();
        for (slug, md, h) in &changed {
            let mut chunks: Vec<Chunk> = Vec::new();
            for (i, text) in chunk_text(md).into_iter().enumerate() {
                let vector = it.next().unwrap_or_default();
                chunks.push(Chunk {
                    slug: slug.clone(),
                    idx: i as u32,
                    doc_hash: *h,
                    text,
                    vector,
                });
            }
            // чанки дока замещаются целиком: усохшие не живут
            self.store
                .index
                .lock()
                .expect("index lock")
                .docs
                .insert(slug.clone(), chunks);
        }
        self.store.save()?;
        tracing::info!(
            event = "semantic.sync",
            docs = changed.len(),
            chunks = inputs.len(),
            vectors = self.embeddings.is_some(),
            "Semantic index synced"
        );
        Ok(changed.len())
    }
}
