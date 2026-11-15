//! Поиск: Ollama-эмбеддинги и LanceDB-бэкенд.

mod semantic;
mod semantic_backend;

pub use semantic::{OllamaEmbeddings, EMBED_MODEL};
pub use semantic_backend::LanceSemanticBackend;
