//! Поиск: гибрид полнотекста (токены по кэшу доков) и семантики
//! (векторный индекс). Семантика строится фоновой дотяжкой: тик
//! индексирует изменённые доки батчами через OpenRouter (free).

use crate::error::UsecaseResult;
use crate::ports::WikiRepo;
use std::sync::Arc;
use uwuwu_domain::UtcMinute;

/// Публичный порт поиска (MCP search).
pub struct SearchUseCase {
    pub wiki: Arc<dyn WikiRepo>,
    pub semantic: Option<Arc<dyn SemanticBackend>>,
    /// Токен OpenRouter (нет — семантика честно офлайн).
    pub openrouter_key: Option<String>,
}

/// Результат поиска: секции как в cli search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub section: &'static str,
    pub slug: String,
    pub score: String,
}

#[async_trait::async_trait]
pub trait SemanticBackend: Send + Sync {
    /// Топ-k семантических попаданий по запросу.
    async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<(f32, String)>>;
    /// Дотяжка индекса: индексирует изменённые доки, возвращает счётчик.
    async fn sync_index(&self, budget: usize) -> UsecaseResult<usize>;
}

impl SearchUseCase {
    /// Гибридный поиск: полнотекст + семантика (если жива).
    pub async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<SearchHit>> {
        let mut out = Vec::new();
        // Полнотекст: токены AND по кэшу дерева + чтение кандидатов
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.len() > 1)
            .collect();
        if tokens.is_empty() {
            return Ok(out);
        }
        let all = self.wiki.list_docs("").await?;
        let mut full_hits = 0;
        for slug in all.iter().take(2000) {
            if full_hits >= top {
                break;
            }
            if !tokens.iter().all(|t| slug.to_lowercase().contains(t)) {
                // быстрый матч по слагу; контентный — семантикой
                continue;
            }
            out.push(SearchHit {
                section: "полнотекст",
                slug: slug.clone(),
                score: "слаг".into(),
            });
            full_hits += 1;
        }
        if let (Some(backend), true) = (&self.semantic, !query.trim().is_empty()) {
            match backend.search(query, top).await {
                Ok(hits) => {
                    for (score, slug) in hits {
                        out.push(SearchHit {
                            section: "семантика",
                            slug,
                            score: format!("{score:.2}"),
                        });
                    }
                }
                Err(e) => {
                    // отказ маршрута — честная строка, не тихая слепота
                    out.push(SearchHit {
                        section: "семантика",
                        slug: format!("— недоступна: {e}"),
                        score: String::new(),
                    });
                }
            }
        }
        Ok(out)
    }
}

/// Пик-окно дотяжки индекса: семантика не жжёт запросы в пик.
pub fn sync_allowed(now: UtcMinute) -> bool {
    now.weekday <= 5 && !(now.minute_of_day >= 360 && now.minute_of_day < 600)
}
