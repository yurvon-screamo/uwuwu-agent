//! DocStore — контракт вики-домена над бекендом: md-доки по слагам,
//! перечень детей папки, blob-операции, источники search.
//!
//! GitHub: md-файлы
//! docs/ репо uwuwu-hub через локальный git-клон (ленивый fetch).

use anyhow::Result;

/// Слаг → путь/страница (GitHub: путь
/// в docs/). Реализация нормализует регистр и слэши.
#[derive(Clone, Debug)]
pub struct DocRef {
    pub slug: String,
}

/// Элемент перечня детей папки (docs ls): имя + признак папки.
#[derive(Clone, Debug)]
pub struct DocEntry {
    pub name: String,
    pub is_folder: bool,
}

/// Blob: ключ = путь/референс носителя (GitHub:
/// assets/ репо или user-attachments URL).
#[derive(Clone, Debug)]
pub struct BlobRef {
    pub key: String,
}

/// реализация различает пустой ответ и честный 404, документировано в
/// AGENTS.md (blob).
#[derive(Debug)]
pub enum BlobGet {
    Bytes(Vec<u8>),
    Missing,
}

/// Источники search-таблицы CLI: контрактовые кортежи, склейку и сортировку
/// делает командный слой (внешние, UI-нейтральные).
#[derive(Clone, Debug)]
pub struct FulltextHit {
    pub title: String,
    pub snippet: String,
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct TitleHit {
    pub slug: String,
    pub title: String,
}

#[async_trait::async_trait]
pub trait DocStore: Send + Sync {
    /// Markdown-экспорт дока по слагу (read).
    async fn read(&self, doc: &DocRef) -> Result<String>;

    /// Дети папки: "Projects" → папки проектов; "Projects/<proj>" → доки.
    async fn ls(&self, path: &str) -> Result<Vec<DocEntry>>;

    /// Полнотекст (code search / grep —
    /// GitHub). Отказ маршрута — Err с честным текстом, не пустой вектор.
    async fn search_fulltext(&self, query: &str, top_n: usize) -> Result<Vec<FulltextHit>>;

    /// Доки по заголовкам (hpath-матч, case-insensitive, AND-токены;
    /// тайтлы ADR резолвит реализация).
    async fn search_by_title(&self, query: &str, top_n: usize) -> Result<Vec<TitleHit>>;

    /// Список blob-ключей префикса (инвентарь ассетов).
    async fn blob_list(&self, prefix: &str) -> Result<Vec<BlobRef>>;

    async fn blob_get(&self, key: &BlobRef) -> Result<BlobGet>;

    /// assets/<имя> в репо — GitHub).
    async fn blob_put(&self, local_path: &std::path::Path, dest_name: &str) -> Result<BlobRef>;
}
