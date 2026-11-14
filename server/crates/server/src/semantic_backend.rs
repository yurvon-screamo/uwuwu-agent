//! Связка SemanticBackend: OpenRouter-эмбеддинги + плоский индекс.
//! Дотяжка инкрементальна (hash дока), батчами, вне пик-окна.

use crate::semantic::{chunk_text, text_hash, Chunk, FileSemanticStore, OpenRouterEmbeddings};
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::ports::WikiRepo;
use uwuwu_usecase::search::SemanticBackend;

pub struct FlatSemanticBackend {
    pub wiki: Arc<dyn WikiRepo>,
    pub store: Arc<FileSemanticStore>,
    pub embeddings: Arc<OpenRouterEmbeddings>,
}

#[async_trait]
impl SemanticBackend for FlatSemanticBackend {
    async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<(f32, String)>> {
        let vecs = self.embeddings.embed_batch(&[query.to_string()]).await?;
        let Some(qv) = vecs.first() else {
            return Err(UsecaseError::State("embedding запроса пуст".into()));
        };
        let index = self.store.index.lock().expect("index lock");
        Ok(index
            .search(qv, top)
            .into_iter()
            .map(|(score, slug, _)| (score, slug))
            .collect())
    }

    async fn sync_index(&self, budget: usize) -> UsecaseResult<usize> {
        let all = self.wiki.list_docs("").await?;
        let mut changed: Vec<(String, String)> = Vec::new();
        for slug in all {
            if changed.len() >= budget {
                break;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                let h = text_hash(&md);
                if self.store.doc_hash(&slug) != Some(h) {
                    changed.push((slug, md));
                }
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        // чанки батчами
        let mut inputs: Vec<String> = Vec::new();
        let mut owners: Vec<(String, u32, u64)> = Vec::new(); // slug, idx, doc-hash
        let mut per_doc: std::collections::HashMap<String, Vec<u32>> = Default::default();
        for (slug, md) in &changed {
            let h = text_hash(md);
            for (i, chunk) in chunk_text(md).into_iter().enumerate() {
                per_doc.entry(slug.clone()).or_default().push(i as u32);
                owners.push((slug.clone(), i as u32, h));
                inputs.push(chunk);
            }
        }
        let vectors = self.embeddings.embed_batch(&inputs).await?;
        for ((slug, idx, h), vector) in owners.into_iter().zip(vectors) {
            let store = &self.store;
            let chunk = Chunk {
                slug: format!("{slug}#{idx}"),
                idx,
                text_head: h.to_string(),
                vector,
            };
            let entry_slug = slug.clone();
            let mut index = store.index.lock().expect("index lock");
            let chunks = index.docs.entry(entry_slug.clone()).or_default();
            if (idx as usize) < chunks.len() {
                chunks[idx as usize] = chunk;
            } else if (idx as usize) == chunks.len() {
                chunks.push(chunk);
            }
        }
        self.store.save()?;
        tracing::info!(
            event = "semantic.sync",
            docs = changed.len(),
            chunks = inputs.len(),
            "Semantic index synced"
        );
        Ok(changed.len())
    }
}
