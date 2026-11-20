//! Поиск: эмбеддинги Gemini Embedding 2 (#1363) и LanceDB-бэкенд
//! (таблица чанков + FTS BM25; dim-guard — прозрачная переиндексация
//! при смене размерности, ADR-108 маршрута туннеля не обслуживает).

mod semantic;
mod semantic_backend;

pub use semantic::{Embedder, GeminiEmbeddings, RATE_LIMIT_CAP_SECS, EMBED_MODEL};
pub use semantic_backend::LanceSemanticBackend;

/// Документов за один тик semantic-sync (#1363): производная замеров спайка —
/// ток-бюджет тика (TPM_факт × pacing × период_тика/60с) против среднего веса
/// дока (~2k токенов), гард 120с сайдкара. Число фиксируется по финальным
/// цифрам спайк-прогона (гейт среза 1); спайк 20.11: 30k TPM-гипотеза × 0.5 ×
/// 3 мин ≈ 45k ток → ~20 доков.
pub const SEMANTIC_SYNC_BUDGET: usize = 20;
