//! Запись текстовых клеток строк досок (Branch, PR и пр.).

use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::Result;
use serde_json::json;

/// Одна text-клетка (keyID от board_keys).
async fn write_text_cell(sy: &SiYuan, av: &str, key: &str, item: &str, value: &str) -> Result<()> {
    sy.mcp_tool(
        "database",
        json!({ "action": "item_update", "id": av, "keyID": key, "itemID": item,
                "value": { "text": { "content": value } } }),
    )
    .await?;
    Ok(())
}

/// Текстовые клетки по именам колонок. Отсутствующая колонка — ошибка:
/// тихий скип маскировал незаписанную ячейку (репро 04.11, задача 20261104220156).
pub(super) async fn set_text_cells(
    sy: &SiYuan,
    av: &str,
    item: &str,
    cells: &[(&str, &str)],
) -> Result<()> {
    let keys = syt::board_keys(sy, av).await?;
    for (name, value) in cells {
        let Some(key) = keys
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| id.as_str())
        else {
            anyhow::bail!("колонка «{name}» не найдена на доске {av} — ячейка не записана");
        };
        write_text_cell(sy, av, key, item, value).await?;
    }
    Ok(())
}

// ——— docs/search/blob/projects ———
