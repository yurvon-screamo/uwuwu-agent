//! Юнит-тесты выдачи поиска: отказ секции — честная строка, не тихая
//! слепота (UC-04, #1473). Фейковый бекенд с настраиваемыми исходами
//! fulltext/search покрывает все формы ответа секции.

use crate::error::{UsecaseError, UsecaseResult};
use crate::search::{SearchHit, SearchUseCase, SemanticBackend};
use async_trait::async_trait;
use std::sync::Arc;

/// Фейковый бекенд: исходы fulltext/search задаются сценарием теста.
struct FakeBackend {
    fulltext: UsecaseResult<Vec<String>>,
    search: UsecaseResult<Vec<(f32, String)>>,
}

#[async_trait]
impl SemanticBackend for FakeBackend {
    async fn search(&self, _query: &str, _top: usize) -> UsecaseResult<Vec<(f32, String)>> {
        self.search.clone()
    }

    async fn fulltext(&self, _tokens: &[String], _top: usize) -> UsecaseResult<Vec<String>> {
        self.fulltext.clone()
    }

    async fn sync_index(&self, _budget: usize) -> UsecaseResult<usize> {
        Ok(0)
    }
}

fn backend_with(fulltext: UsecaseResult<Vec<String>>) -> SearchUseCase {
    SearchUseCase {
        semantic: Some(Arc::new(FakeBackend {
            fulltext,
            search: Ok(vec![(0.9, "Projects/uwuwu/README".to_string())]),
        })),
    }
}

#[tokio::test]
async fn fulltext_failure_renders_honest_section_line() {
    let uc = backend_with(Err(UsecaseError::State("lance: fts: сломано".into())));

    let hits = uc.search("лимиты крона", 5).await.expect("search ok");

    let fulltext_line = hits
        .iter()
        .find(|hit| hit.section == "fulltext")
        .expect("секция полнотекста присутствует при отказе");
    assert!(
        fulltext_line.slug.contains("unavailable") && fulltext_line.slug.contains("сломано"),
        "отказ рендерится строкой с причиной, факт: {fulltext_line:?}"
    );
}

#[tokio::test]
async fn fulltext_hits_render_as_section_entries() {
    let uc = backend_with(Ok(vec!["Projects/uwuwu/README".to_string()]));

    let hits = uc.search("лимиты крона", 5).await.expect("search ok");

    let found = hits
        .iter()
        .find(|hit| hit.section == "fulltext" && hit.slug == "Projects/uwuwu/README");
    assert!(found.is_some(), "попадание полнотекста в выдаче: {hits:?}");
}

#[tokio::test]
async fn semantic_failure_renders_honest_section_line() {
    let uc = SearchUseCase {
        semantic: Some(Arc::new(FakeBackend {
            fulltext: Ok(vec![]),
            search: Err(UsecaseError::RateLimited {
                service: "gemini-embeddings".into(),
                retry_after_secs: Some(30508),
            }),
        })),
    };

    let hits = uc.search("лимиты крона", 5).await.expect("search ok");

    let semantic_line = hits
        .iter()
        .find(|hit: &&SearchHit| hit.section == "semantic")
        .expect("секция семантики присутствует при отказе");
    assert!(
        semantic_line.slug.contains("unavailable") && semantic_line.slug.contains("30508"),
        "отказ с retry-after виден в строке, факт: {semantic_line:?}"
    );
}
