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

/// Документов за один тик semantic-sync: емкостный гард, а не квотный
/// (#1363 → #1687). Лимита по времени нет — тик идёт до завершения (#1730);
/// остальные сайдкары ограничены `[pipeline] sidecar_timeout_ms`.
/// Значение — из `[search] semantic_sync_budget` config.toml (uwuwu#1726).
pub fn semantic_sync_budget() -> usize {
    uwuwu_usecase::runtime_cfg::search::budget()
}
