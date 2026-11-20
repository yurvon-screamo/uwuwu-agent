//! Поиск: эмбеддинги Gemini Embedding 2 (#1363) и LanceDB-бэкенд
//! (таблица чанков + FTS BM25; dim-guard — прозрачная переиндексация
//! при смене размерности, ADR-108 маршрута туннеля не обслуживает).

mod semantic;
mod semantic_backend;

pub use semantic::{
    Embedder, GeminiEmbeddings, RATE_LIMIT_CAP_SECS, RATE_LIMIT_DEFAULT_SECS, EMBED_MODEL,
};
pub use semantic_backend::LanceSemanticBackend;

/// Документов за один тик semantic-sync (#1363): производная замеров спайка —
/// ток-бюджет тика (TPM-гипотеза 30k × pacing 0.5 × период 180с ≈ 45k ток)
/// против фактического веса дока (спайк: 5.5 чанков × 416 ток ≈ 2 288 ток)
/// → floor 19; гард 120с сайдкара — латенс батчей спайка median 2.7s / max 3.9s
/// даёт ~15–20с на тик, запас ×6 (гейт среза 1, тело задачи).
pub const SEMANTIC_SYNC_BUDGET: usize = 19;
