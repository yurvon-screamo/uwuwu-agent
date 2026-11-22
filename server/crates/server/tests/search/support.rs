//! Общий бойлерплейк интеграционных тестов семантики: фейк вики-репо,
//! счётный/фиксированный мок-эмбеддер, временный Lance-том с гардом уборки.
//! Дублировать это в каждом тест-файле — identical-code smell (#1473).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use uwuwu_usecase::{AppendDocOutcome, UsecaseResult, WikiRepo};

use super::semantic::{Embedder, EMBED_MODEL_DIMS};

/// Фейк вики-репо: (markdown, git-sha) по слагу.
pub struct FakeWiki {
    pub docs: HashMap<String, (String, String)>,
}

#[async_trait]
impl WikiRepo for FakeWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self.docs.get(slug).map(|(md, _)| md.clone()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(self
            .docs
            .keys()
            .filter(|slug| slug.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(self
            .docs
            .iter()
            .map(|(slug, (_, sha))| (slug.clone(), sha.clone()))
            .collect())
    }

    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn append_doc(
        &self,
        _slug: &str,
        _line: &str,
        _message: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        Ok(AppendDocOutcome {
            created: false,
            lines: 0,
        })
    }
}

/// Фиксированный мок-эмбеддер: нулевые векторы нужной размерности.
/// Эмбеддинги — внешний unmanaged-dep, мок трейта — штатный приём тестов.
pub struct FixedEmbedder;

#[async_trait]
impl Embedder for FixedEmbedder {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        Ok(docs
            .iter()
            .map(|_| vec![0.0f32; EMBED_MODEL_DIMS])
            .collect())
    }

    async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
        Ok(vec![0.0f32; EMBED_MODEL_DIMS])
    }
}

/// Lance-том в уникальной временной папке (без новой dev-dep tempfile):
/// наносекунда в суффиксе разводит параллельные тесты одного процесса.
pub fn lance_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "uwuwu-semantic-{label}-{}-{nanos}",
        std::process::id()
    ))
}

/// Guard чистит папку при любом исходе теста, паника включительно —
/// падающие прогоны не копят мусор в /tmp (замечание ревью #1392).
pub struct TempDirGuard(pub PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
