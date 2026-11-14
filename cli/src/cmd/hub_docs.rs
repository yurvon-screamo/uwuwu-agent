//! Hub-ветка doc-семьи (read/docs ls/search): DocStore GitHubDocStore
//! (git-клон uwuwu-hub). Перехват в run() по UWUWU_HUB_REPO. Срез D,
//! задача 20261112155302-ttyp35g.

use crate::out::outln;
use crate::store::doc_store::{DocRef, DocStore};
use crate::store::github_doc_store::GitHubDocStore;
use anyhow::Result;

fn doc_store() -> GitHubDocStore {
    GitHubDocStore::from_env(crate::cmd::run::hub_repo_default())
}

pub(crate) async fn read(slug: &str) -> Result<String> {
    doc_store()
        .read(&DocRef {
            slug: slug.to_string(),
        })
        .await
}

pub(crate) async fn docs_ls(prefix: &str) -> Result<()> {
    let entries = doc_store().ls(prefix).await?;
    if entries.is_empty() {
        outln!("пусто");
        return Ok(());
    }
    for e in &entries {
        outln!("{}", e.name);
    }
    Ok(())
}

/// Search hub: полнотекст (rg/grep по клону) + заголовки (hpath по путям)
/// — две секции top-n, semantic — вниз (старая машина Ollama поверх
/// того же индекса, дельта из hub).
pub(crate) async fn search(query: &str, limit: usize, threshold: f32) -> Result<()> {
    let store = doc_store();
    let fulltext = store.search_fulltext(query, limit).await?;
    let titles = store.search_by_title(query, limit).await?;

    outln!("Полнотекст (git-клон uwuwu-hub):");
    if fulltext.is_empty() {
        outln!("  — пусто");
    }
    for h in &fulltext {
        outln!("  — {}: {}", h.title, h.snippet);
    }
    outln!("Доки по заголовкам:");
    if titles.is_empty() {
        outln!("  — пусто");
    }
    for t in &titles {
        outln!("  — {}", t.slug);
    }
    // Semantic-секция: индекс per-машинный; дельта в остатке бюджета —
    // hub-источник (semantic::search_half с None-sy). Деградация секции —
    // честный статус, команда не падает.
    let cfg = crate::semantic::SemanticConfig::from_env();
    let section = crate::semantic::search_half(&cfg, query, limit, threshold).await;
    outln!();
    for line in section {
        outln!("{line}");
    }
    Ok(())
}
