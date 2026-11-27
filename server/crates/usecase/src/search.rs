//! Поиск: гибрид полнотекста (токены по кэшу доков) и семантики
//! (векторный индекс). Семантика строится фоновой дотяжкой: тик
//! индексирует изменённые доки батчами через локальную Ollama (#1687).

use crate::error::UsecaseResult;
use std::sync::Arc;

/// Публичный порт поиска (MCP search).
pub struct SearchUseCase {
    pub semantic: Option<Arc<dyn SemanticBackend>>,
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
    /// Полнотекст по контенту чанков (токены AND).
    async fn fulltext(&self, tokens: &[String], top: usize) -> UsecaseResult<Vec<String>>;
    /// Дотяжка индекса: индексирует изменённые доки, возвращает счётчик.
    async fn sync_index(&self, budget: usize) -> UsecaseResult<usize>;
}

impl SearchUseCase {
    /// Гибридный поиск: полнотекст по контенту + семантика (если жива).
    pub async fn search(&self, query: &str, top: usize) -> UsecaseResult<Vec<SearchHit>> {
        let mut out = Vec::new();
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.len() > 1)
            .collect();
        if tokens.is_empty() {
            return Ok(out);
        }
        if let Some(backend) = &self.semantic {
            match backend.fulltext(&tokens, top).await {
                Ok(slugs) => {
                    for slug in slugs {
                        out.push(SearchHit {
                            section: "fulltext",
                            slug,
                            score: "AND tokens".into(),
                        });
                    }
                }
                Err(e) => out.push(SearchHit {
                    section: "fulltext",
                    slug: format!("— unavailable: {e}"),
                    score: String::new(),
                }),
            }
        } else {
            out.push(SearchHit {
                section: "fulltext",
                slug: "— the index is not built".into(),
                score: String::new(),
            });
        }
        if !query.trim().is_empty() {
            match &self.semantic {
                Some(backend) => match backend.search(query, top).await {
                    Ok(hits) => {
                        for (score, slug) in hits {
                            out.push(SearchHit {
                                section: "semantic",
                                slug,
                                score: format!("{score:.2}"),
                            });
                        }
                    }
                    Err(e) => out.push(SearchHit {
                        section: "semantic",
                        slug: format!("— unavailable: {e}"),
                        score: String::new(),
                    }),
                },
                None => out.push(SearchHit {
                    section: "semantic",
                    slug: "— the index is not built".into(),
                    score: String::new(),
                }),
            }
        }
        Ok(out)
    }
}
