//! Резолв слага `read` в docId ядра SiYuan.
//!
//! Формы слага: `<project>/<doc>` (ноутбук Projects), `<topic>/<doc>`
//! (Experience), одиночный слаг дневника (Daily), одиночный заголовок дока.
//! Алгоритм: точный hpath-матч `/{slug}` по всем ноутбукам; при нуле —
//! фолбэк по заголовку (последний сегмент слага, прежнее поведение);
//! неоднозначность — ошибка с перечнем кандидатов (hpath, ноутбук, docId —
//! docId даёт агенту готовый обход find → export).

use crate::siyuan::SiYuan;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Слаг → docId дока (export по нему делает вызывающий).
pub(crate) async fn resolve(sy: &SiYuan, slug: &str) -> Result<String> {
    // 1) точный матч слага как полного hpath: /<project>/<doc>, /<topic>/<doc>, /<daily-slug>
    let mut exact = doc_blocks(sy, &format!("hpath = {}", sql_str(&format!("/{slug}")))).await?;
    if exact.len() > 1 {
        return Err(anyhow!(ambiguous(slug, &exact, sy).await));
    }
    if let Some(doc) = exact.pop() {
        return Ok(doc.id);
    }

    // 2) фолбэк: последний сегмент слага = заголовок дока
    let title = slug.rsplit('/').next().unwrap_or(slug);
    let mut by_title = doc_blocks(sy, &format!("content = {}", sql_str(title))).await?;
    match by_title.len() {
        1 => Ok(by_title.pop().expect("len checked above").id),
        0 => Err(anyhow!("док «{slug}» не найден")),
        _ => Err(anyhow!(ambiguous(slug, &by_title, sy).await)),
    }
}

struct DocHit {
    id: String,
    hpath: String,
    box_id: String,
}

/// Док-блоки (type='d') по готовому WHERE-фрагменту.
async fn doc_blocks(sy: &SiYuan, condition: &str) -> Result<Vec<DocHit>> {
    let rows = sy
        .sql(&format!(
            "select id, hpath, box from blocks where type='d' and {condition}"
        ))
        .await?;
    Ok(rows.iter().map(doc_hit).collect())
}

fn doc_hit(row: &Value) -> DocHit {
    DocHit {
        id: str_field(row, "id"),
        hpath: str_field(row, "hpath"),
        box_id: str_field(row, "box"),
    }
}

/// Сообщение об неоднозначности: каждый кандидат с hpath, именем ноутбука и docId.
async fn ambiguous(slug: &str, hits: &[DocHit], sy: &SiYuan) -> String {
    let names = notebook_names(sy).await.unwrap_or_default();
    let mut msg = format!("док «{slug}» неоднозначен ({} кандидатов):", hits.len());
    for hit in hits {
        let notebook = names.get(&hit.box_id).map(String::as_str).unwrap_or("?");
        msg.push_str(&format!(
            "\n  - {} (ноутбук: {}, id: {})",
            hit.hpath, notebook, hit.id
        ));
    }
    msg
}

/// id ноутбука → имя (для перечня кандидатов).
async fn notebook_names(sy: &SiYuan) -> Result<HashMap<String, String>> {
    let mut names = HashMap::new();
    for nb in sy.notebooks().await? {
        if let (Some(id), Some(name)) = (
            nb.get("id").and_then(|v| v.as_str()),
            nb.get("name").and_then(|v| v.as_str()),
        ) {
            names.insert(id.to_string(), name.to_string());
        }
    }
    Ok(names)
}

/// Строка в SQL-литерал с экранированием одинарной кавычки.
fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn str_field(row: &Value, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_str_escapes_single_quotes() {
        assert_eq!(sql_str("it's"), "'it''s'");
        assert_eq!(sql_str("plain"), "'plain'");
    }
}
