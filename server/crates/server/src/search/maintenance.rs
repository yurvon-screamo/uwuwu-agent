//! Обслуживание Lance-таблиц после чурна синка (#1436): компакция мелких
//! фрагментов + prune старых версий/файлов. Без обслуживания чурн
//! delete+add каждого тика копит манифесты/фрагменты/deletion-файлы
//! бесконечно — прод-факт #1436: линейный anon-рост кучи, read_version
//! таблицы до 3946, кэши метаданных по путям файлов.

use std::time::Duration;

use lancedb::table::{CompactionOptions, OptimizeAction, Table};
use uwuwu_usecase::UsecaseResult;

use super::semantic_backend::err;

/// Ретеншен прод-обслуживания (тик синка = 180с). Минуты, не секунды:
/// конкурентный MCP-поиск читает актуальную версию, окно ретеншена
/// много больше длительности запроса (решение по конкурентности — план #1436).
pub(crate) const PROD_RETENTION: Duration = Duration::from_secs(180);

/// Компакция + prune одной таблицы. `delete_unverified: true` — том
/// однопроцессный (Railway-сервис один; стенды/e2e — tempdir), WARNING
/// крейта покрыт гарантией: без флага prune не удаляет unverified-файлы
/// моложе 7 дней и «ретеншен — минуты» не чистит файлы, а с ними кэши
/// метаданных по путям (половина механизма #1436).
pub(crate) async fn maintain_table(
    table: Table,
    label: &str,
    older_than: Duration,
) -> UsecaseResult<()> {
    let compaction = table
        .optimize(OptimizeAction::Compact {
            options: CompactionOptions::default(),
            remap_options: None,
        })
        .await
        .map_err(|e| err(format!("compact {label}: {e}")))?;
    let prune_retention = chrono::Duration::from_std(older_than)
        .map_err(|e| err(format!("retention {label}: {e}")))?;
    let prune = table
        .optimize(OptimizeAction::Prune {
            older_than: Some(prune_retention),
            delete_unverified: Some(true),
            error_if_tagged_old_versions: None,
        })
        .await
        .map_err(|e| err(format!("prune {label}: {e}")))?;
    let version = table
        .version()
        .await
        .map_err(|e| err(format!("version {label}: {e}")))?;
    tracing::info!(
        event = "semantic.maintain",
        table = label,
        version = version,
        compaction = ?compaction.compaction,
        prune = ?prune.prune,
        "Lance table maintained"
    );
    Ok(())
}
