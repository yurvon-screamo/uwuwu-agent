//! Hub-синк semantic-индекса: full-скан uwuwu-hub → общий конвейер
//! process_page/embed_batches (skip-unchanged по (updated, content),
//! prune усохших/переросших, drift путей) + prune исчезнувших hub-строк.
//! Watermark не используется — полный скан дедупится содержимым.

use super::hub_source::{hub_rows, stale_hub_ids};
use super::ollama::OllamaClient;
use super::store::Store;
use super::sync::{embed_batches, process_page, SyncOptions, SyncReport};
use crate::store::github_doc_store::GitHubDocStore;
use anyhow::Result;

pub(crate) async fn hub_sync(
    doc_store: &GitHubDocStore,
    ollama: &OllamaClient,
    store: &mut Store,
    opts: &SyncOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let (rows, by_id) = hub_rows(doc_store).await?;
    let page_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    let indexed = store.indexed_rows(&page_ids)?;

    let outcome = process_page(&rows, &by_id, &indexed);
    report.skipped_short += outcome.skipped_short;
    report.skipped_long += outcome.skipped_long;
    let mut pruned = outcome.prune_ids.clone();
    if !outcome.prune_ids.is_empty() {
        report.pruned += store.prune(&outcome.prune_ids)?;
    }
    if !outcome.drift.is_empty() {
        report.paths_refreshed += store.refresh_paths(&outcome.drift)?;
    }

    // Исчезнувшие hub-строки (файл снесён в hub): индексная строка с
    // box hub-*, которой нет в скане — prune. Сверка по chunk_metas.
    let stale = stale_hub_ids(&store.chunk_metas()?, &rows);
    if !stale.is_empty() {
        report.pruned += store.prune(&stale)?;
        pruned.extend(stale);
    }

    embed_batches(
        ollama,
        store,
        &super::sync::embed_batch_windows(&outcome.pending),
        opts,
        &mut report,
    )
    .await?;
    Ok(report)
}
