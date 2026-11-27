//! Поиск: эмбеддинги локальной Ollama embeddinggemma-2 (#1687) и
//! LanceDB-бэкенд (таблица чанков + FTS BM25; dim-guard — прозрачная
//! переиндексация при смене размерности).

mod fts;
mod maintenance;
mod query;
mod semantic;
mod semantic_backend;

pub use semantic::{embed_model, Embedder, OllamaEmbeddings};
pub use semantic_backend::LanceSemanticBackend;

/// Документов за один тик semantic-sync: емкостный гард (120 с сайдкара и
/// соседние модели владельца на той же Ollama), а не квотный (#1363 → #1687).
/// Реально за тик укладывается `min(512, сколько успевается за 120 с)`.
pub fn semantic_sync_budget() -> usize {
    uwuwu_usecase::runtime_cfg::search::budget()
}
