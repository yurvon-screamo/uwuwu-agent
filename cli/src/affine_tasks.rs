//! `uwuwu-cli task` — CRUD канбан-задач на AFFiNE.
//!
//! Модель (см. док «Фабрика разработки на AFFiNE»): задача = строка
//! database в доске проекта (`<Proj> Tasks`) + линкованный док-тело с
//! тегом «таск». Комменты — нативные комменты афины к доку-телу.

use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::{json, Value};

use crate::affine_client::{AffineClient, AffineConfig};

/// Доска проекта: док + database-блок внутри него.
pub struct Board {
    pub doc_id: String,
    pub db_id: String,
}

/// Найти доску проекта: док с тайтлом `<project> Tasks` → его database-блок.
pub async fn find_board(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    project: &str,
) -> Result<Board> {
    let title = format!("{project} Tasks");
    let resp = client
        .call_tool(
            "find_doc_by_title",
            json!({"workspaceId": cfg.workspace, "title": title, "caseInsensitive": true, "limit": 1}),
        )
        .await
        .context("find_doc_by_title")?;
    let doc_id = resp
        .get("matches")
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.first())
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| first_str(&resp, &["docId", "id", "doc_id"]))
        .ok_or_else(|| {
            anyhow!("доска «{title}» не найдена (ожидается док <Proj> Tasks с канбаном)")
        })?;
    let doc = client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": doc_id}),
        )
        .await
        .context("read_doc board")?;
    let blocks = doc
        .get("blocks")
        .and_then(|b| b.as_array())
        .ok_or_else(|| anyhow!("доска «{title}»: пустой read_doc"))?;
    let db_id = blocks
        .iter()
        .find(|b| {
            let f = b.get("flavour").and_then(|v| v.as_str()).unwrap_or("");
            f == "affine:database" || f == "affine:data_view"
        })
        .and_then(|b| b.get("id").and_then(|v| v.as_str()))
        .ok_or_else(|| anyhow!("в доке «{title}» нет database-блока"))?
        .to_string();
    Ok(Board { doc_id, db_id })
}

/// Одна строка доски.
#[derive(Clone, Debug, Default)]
pub struct TaskRow {
    pub row_id: String,
    pub title: String,
    pub body_doc: String,
    pub status: String,
    pub pr: String,
    pub branch: String,
}

/// Прочитать все строки доски.
pub async fn read_rows(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
) -> Result<Vec<TaskRow>> {
    let resp = client
        .call_tool(
            "read_database_cells",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id, "databaseBlockId": board.db_id}),
        )
        .await
        .context("read_database_cells")?;
    let rows = resp
        .get("rows")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let cells = r.get("cells").cloned().unwrap_or(Value::Null);
        let cell = |k: &str| -> String {
            cells
                .get(k)
                .and_then(|c| c.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let linked = first_str(&r, &["linkedDocId"]).unwrap_or_default();
        let mut title = r
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(" ")
            .trim()
            .to_string();
        // linked rows carry their title on the linked page, not the cell
        if title.is_empty() && !linked.is_empty() {
            if let Ok(doc) = client
                .call_tool(
                    "get_doc",
                    json!({"workspaceId": cfg.workspace, "docId": linked}),
                )
                .await
            {
                title = doc
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
            }
        }
        out.push(TaskRow {
            row_id: first_str(&r, &["rowBlockId", "rowId", "id"]).unwrap_or_default(),
            title,
            body_doc: linked,
            status: cell("Status"),
            pr: cell("PR"),
            branch: cell("Branch"),
        });
    }
    Ok(out)
}

/// Найти строку по id (row id или doc-тела) или уникальному префиксу.
/// Ведущие дефисы row-id игнорируются с обеих сторон (кли-аргументы).
pub fn resolve_row<'a>(rows: &'a [TaskRow], id: &str) -> Result<&'a TaskRow> {
    let needle = id.trim_start_matches('-');
    let by_id: Vec<&TaskRow> = rows
        .iter()
        .filter(|r| {
            r.row_id.trim_start_matches('-').starts_with(needle) || r.body_doc.starts_with(needle)
        })
        .collect();
    if by_id.len() == 1 {
        return Ok(by_id[0]);
    }
    if by_id.is_empty() {
        let lower = needle.to_lowercase();
        let by_title: Vec<&TaskRow> = rows
            .iter()
            .filter(|r| r.title.to_lowercase().contains(&lower))
            .collect();
        return match by_title.len() {
            1 => Ok(by_title[0]),
            0 => bail!("задача '{id}' не найдена"),
            n => bail!("'{id}' неоднозначно ({n} по заголовку) — уточни или используй id"),
        };
    }
    bail!(
        "'{id}' неоднозначно ({} по id) — уточни префикс",
        by_id.len()
    )
}

/// Создать задачу: док-тело (тег «таск») + строка Todo, слинкованная с телом.
pub async fn create_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
    title: &str,
    body_markdown: &str,
    attach_paths: &[std::path::PathBuf],
) -> Result<TaskRow> {
    // 1. док-тело — ребёнок доски
    let md = if body_markdown.trim().is_empty() {
        format!("{title}\n")
    } else {
        format!("{title}\n\n{body_markdown}\n")
    };
    let resp = client
        .call_tool(
            "create_doc_from_markdown",
            json!({"workspaceId": cfg.workspace, "title": title, "markdown": md, "parentDocId": board.doc_id}),
        )
        .await
        .context("create_doc_from_markdown")?;
    let body_doc = first_str(&resp, &["docId", "id"])
        .ok_or_else(|| anyhow!("тело создано, но docId не возвращён: {resp:.120}"))?;

    // 2. тег «таск» (идемпотентно)
    let tags = client
        .call_tool("list_tags", json!({"workspaceId": cfg.workspace}))
        .await;
    let has_tag = tags
        .ok()
        .and_then(|t| {
            t.get("tags").and_then(|a| a.as_array()).map(|arr| {
                arr.iter().any(|x| {
                    x.get("name").and_then(|n| n.as_str()) == Some("таск")
                        || x.get("tag").and_then(|n| n.as_str()) == Some("таск")
                })
            })
        })
        .unwrap_or(false);
    if !has_tag {
        let _ = client
            .call_tool(
                "create_tag",
                json!({"workspaceId": cfg.workspace, "tag": "таск"}),
            )
            .await;
    }
    let _ = client
        .call_tool(
            "add_tag_to_doc",
            json!({"workspaceId": cfg.workspace, "docId": body_doc, "tag": "таск"}),
        )
        .await;

    // 3. строка в Todo, слинкованная с телом
    let resp = client
        .call_tool(
            "add_database_row",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                   "databaseBlockId": board.db_id,
                   "cells": {"Title": title, "Status": "Todo"},
                   "linkedDocId": body_doc}),
        )
        .await
        .context("add_database_row")?;
    let row_id = first_str(&resp, &["rowBlockId", "rowId", "id"]).unwrap_or_default();

    // 4. аттачи в тело
    if !attach_paths.is_empty() {
        crate::affine_blob::attach_files(cfg, &body_doc, attach_paths).await?;
    }

    Ok(TaskRow {
        row_id,
        title: title.to_string(),
        body_doc,
        status: "Todo".into(),
        ..Default::default()
    })
}

/// Обновить ячейки строки (Status/Title/PR/Branch — только непустые).
/// Патч колонок строки; пустой = нечего обновлять.
#[derive(Default)]
pub struct RowPatch {
    pub status: Option<String>,
    pub title: Option<String>,
    pub pr: Option<String>,
    pub branch: Option<String>,
}

impl RowPatch {
    fn is_empty(&self) -> bool {
        self.status.is_none() && self.title.is_none() && self.pr.is_none() && self.branch.is_none()
    }

    fn cells(&self) -> Value {
        let mut cells = serde_json::Map::new();
        if let Some(v) = &self.status {
            cells.insert("Status".into(), json!(v));
        }
        if let Some(v) = &self.title {
            cells.insert("Title".into(), json!(v));
        }
        if let Some(v) = &self.pr {
            cells.insert("PR".into(), json!(v));
        }
        if let Some(v) = &self.branch {
            cells.insert("Branch".into(), json!(v));
        }
        Value::Object(cells)
    }
}

/// Обновить ячейки строки (только непустые поля патча).
pub async fn edit_row(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
    row: &TaskRow,
    patch: &RowPatch,
) -> Result<()> {
    if patch.is_empty() {
        bail!("нечего обновлять: передай --status/--title/--pr/--branch");
    }
    let cells = patch.cells();
    client
        .call_tool(
            "update_database_row",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                   "databaseBlockId": board.db_id, "rowBlockId": row.row_id,
                   "cells": cells}),
        )
        .await
        .map(|_| ())
        .context("update_database_row")
}

/// Коммент в док-тело задачи.
pub async fn comment_task(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &TaskRow,
    text: &str,
) -> Result<String> {
    if row.body_doc.is_empty() {
        bail!("у задачи нет док-тела — комментить некуда");
    }
    let resp = client
        .call_tool(
            "create_comment",
            json!({"workspaceId": cfg.workspace, "docId": row.body_doc, "content": text}),
        )
        .await
        .context("create_comment")?;
    Ok(first_str(&resp, &["commentId", "id"]).unwrap_or_default())
}

/// Последние комменты тела задачи (свежие первыми).
pub async fn recent_comments(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    row: &TaskRow,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    if row.body_doc.is_empty() {
        return Ok(Vec::new());
    }
    let resp = client
        .call_tool(
            "list_comments",
            json!({"workspaceId": cfg.workspace, "docId": row.body_doc, "first": limit as u64}),
        )
        .await
        .context("list_comments")?;
    let mut out = Vec::new();
    if let Some(edges) = resp.get("edges").and_then(|e| e.as_array()) {
        for e in edges {
            let node = e.get("node").cloned().unwrap_or(Value::Null);
            let content = node.get("content").cloned().unwrap_or(Value::Null);
            let preview = content
                .get("preview")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            let resolved = node
                .get("resolved")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let marker = if resolved { "[✓] " } else { "" };
            out.push((
                format!("{marker}{preview}"),
                first_str(&node, &["id"]).unwrap_or_default(),
            ));
        }
    }
    Ok(out)
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(row_id: &str, body: &str, title: &str) -> TaskRow {
        TaskRow {
            row_id: row_id.into(),
            body_doc: body.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_by_prefix() {
        let rows = vec![
            row("abc123", "docX", "первая"),
            row("def456", "docY", "вторая"),
        ];
        assert_eq!(resolve_row(&rows, "abc").unwrap().title, "первая");
        assert_eq!(resolve_row(&rows, "docY").unwrap().title, "вторая");
        assert!(resolve_row(&rows, "zzz").is_err());
    }

    #[test]
    fn resolve_ambiguous() {
        let rows = vec![row("ab1", "", ""), row("ab2", "", "")];
        assert!(resolve_row(&rows, "ab").is_err());
    }
}
