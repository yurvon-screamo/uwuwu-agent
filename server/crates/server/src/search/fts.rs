//! Управление FTS-индексом полнотекста (#1473): инвертированный индекс на
//! тексте чанков — без него ланс не даёт брутфорс-план и полнотекст молча
//! пуст (корень «мёртвого полнотекста», квото-независимый). Создаётся один
//! раз при открытии бекенда; дописанные после индексации строки покрывает
//! flat-путь ланса.

use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::{Index, IndexType};
use lancedb::table::Table;

use super::semantic_backend::err;
use uwuwu_usecase::UsecaseResult;

/// Гард и создание FTS-индекса на колонке «text». Отказ не роняет открытие
/// бекенда: семантика и sync живут, полнотекст честно пуст до следующего
/// бута (громкий warn вместо тишины).
pub(super) async fn ensure_fts_index(table: &Table) -> UsecaseResult<bool> {
    let has_fts = table
        .list_indices()
        .await
        .map_err(|e| err(format!("list indices: {e}")))?
        .iter()
        .any(|i| matches!(i.index_type, IndexType::FTS) && i.columns.iter().any(|c| c == "text"));
    if has_fts {
        return Ok(false);
    }
    table
        .create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await
        .map_err(|e| err(format!("create fts index: {e}")))?;
    tracing::info!(
        event = "semantic.fts_index_created",
        "FTS-индекс создан — полнотекст жив"
    );
    Ok(true)
}
