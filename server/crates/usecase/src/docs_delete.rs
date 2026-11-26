//! Удаление вики-дока через MCP (#1658): живое чтение → бэкап → снос.
//! Чистый гард и оркестрация живут здесь; presentation только форматирует
//! квитанцию. Порядок «бэкап раньше мутации» — инвариант этого слоя.

use std::sync::Arc;

use crate::slug_guard::{self, SlugVerdict};
use crate::{
    BlobPlace, BlobUseCase, DocDeleteCommit, PostCheck, UsecaseError, UsecaseResult, WikiRepo,
};

/// Квитанция удаления: всё, что нужно для честного ответа и отката.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReceipt {
    pub slug: String,
    pub path: String,
    pub old_blob_sha: String,
    pub commit_sha: String,
    pub backup_key: String,
    pub backup_url: String,
    pub post_check: PostCheck,
    pub suffix_twins: Vec<String>,
}

/// Отказ гарда до чтения: корень вики и родитель-файл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteRefusal {
    RootPrefix(Vec<String>),
    ParentCollision(String),
}

/// Суффиксный тёзка для удаления неоднозначности не создаёт — он идёт
/// справочной строкой, а не отказом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteGuard {
    pub refusal: Option<DeleteRefusal>,
    pub twins: Vec<String>,
}

pub fn guard(slug: &str, live: &[String]) -> DeleteGuard {
    match slug_guard::check(slug, live) {
        SlugVerdict::RootPrefix(candidates) => DeleteGuard {
            refusal: Some(DeleteRefusal::RootPrefix(candidates)),
            twins: Vec::new(),
        },
        SlugVerdict::ParentCollision(parent) => DeleteGuard {
            refusal: Some(DeleteRefusal::ParentCollision(parent)),
            twins: Vec::new(),
        },
        SlugVerdict::SuffixCollision(twins) => DeleteGuard {
            refusal: None,
            twins,
        },
        SlugVerdict::Clear => DeleteGuard {
            refusal: None,
            twins: Vec::new(),
        },
    }
}

/// Вход оркестрации: один value-тип вместо четырёх позиционных аргументов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRequest {
    pub slug: String,
    pub project: String,
    pub message: String,
    pub twins: Vec<String>,
}

pub struct DocsDeleteUseCase {
    wiki: Arc<dyn WikiRepo>,
    blobs: Arc<BlobUseCase>,
}

impl DocsDeleteUseCase {
    pub fn new(wiki: Arc<dyn WikiRepo>, blobs: Arc<BlobUseCase>) -> Self {
        Self { wiki, blobs }
    }

    /// Живое чтение → бэкап → ff-only удаление. Любой отказ до коммита
    /// оставляет вики нетронутой; гонка ref не ретраится.
    pub async fn delete(&self, request: DeleteRequest) -> UsecaseResult<DeleteReceipt> {
        let snapshot = self
            .wiki
            .read_doc_with_sha(&request.slug)
            .await
            .map_err(|e| UsecaseError::DocReadFailed(e.to_string()))?
            .ok_or_else(|| UsecaseError::DocAbsent(request.slug.clone()))?;
        if snapshot.blob_sha.trim().is_empty() {
            return Err(UsecaseError::DocReadFailed(
                "live blob sha is missing".to_string(),
            ));
        }
        let backup = self
            .blobs
            .place(
                &BlobPlace {
                    project: request.project.clone(),
                    task: None,
                    filename: format!("{}.md", request.slug),
                },
                snapshot.content.as_bytes(),
            )
            .await
            .map_err(|e| UsecaseError::BackupFailed(e.to_string()))?;
        let commit: DocDeleteCommit = self
            .wiki
            .delete_doc(&request.slug, &snapshot.blob_sha, &request.message)
            .await?;
        Ok(DeleteReceipt {
            path: format!("docs/{}.md", request.slug),
            slug: request.slug,
            old_blob_sha: snapshot.blob_sha,
            commit_sha: commit.commit_sha,
            backup_key: backup.key,
            backup_url: backup.url,
            post_check: commit.post_check,
            suffix_twins: request.twins,
        })
    }
}

#[cfg(test)]
#[path = "docs_delete_tests.rs"]
mod docs_delete_tests;
