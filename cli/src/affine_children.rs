//! Резолв существующего тела задачи на доске — фундамент идемпотентного
//! создания (`task create` / `cron set`). Ретрай после клиентского таймаута
//! обязан не плодить дубли: сирота (тело без строки) достраивается, живая
//! строка с тем же тайтлом — явная ошибка.
//!
//! Определение сироты — линк-основанное: ребёнок доски с совпавшим тайтлом,
//! на который НЕ ссылается ни одна строка (`row.body_doc`). Тайтл-проверка
//! строк и линк-проверка тел — независимые барьеры: переименованная задача
//! («тайтл строки ≠ тайтл тела») не превращается в сироту.

use anyhow::{bail, Context as _, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_tasks::{read_rows, Board, TaskCx, TaskRow};

/// Ребёнок дока-доски (list_children).
#[derive(Clone, Debug, PartialEq)]
pub struct BoardChild {
    pub doc_id: String,
    pub title: String,
    pub in_trash: bool,
}

/// Что уже есть на доске с этим тайтлом.
#[derive(Clone, Debug)]
pub enum ExistingBody {
    /// ни живой строки с таким тайтлом, ни свободного живого ребёнка —
    /// чистое создание
    None,
    /// сирота: можно достраивать (перезаписать тело, тег, строку)
    Orphan { doc_id: String },
    /// живая строка с этим тайтлом уже есть — дубль; тело может быть и не
    /// ребёнком доски (карточка из UI без linkedDoc, перенесённое тело)
    Exists { row: TaskRow, row_count: usize },
    /// несколько живых НЕлинкованных детей с этим тайтлом — мусор, ручная
    /// чистка (занятые линком кандидаты исключаются до этого подсчёта)
    Ambiguous { doc_ids: Vec<String> },
}

/// Разобрать ответ list_children; записи без docId — мусор шлюза, мимо.
pub fn parse_children(resp: &serde_json::Value) -> Vec<BoardChild> {
    resp.get("children")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let doc_id = c.get("docId")?.as_str()?.to_string();
                    if doc_id.is_empty() {
                        return None;
                    }
                    Some(BoardChild {
                        doc_id,
                        title: c
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                        in_trash: c.get("inTrash").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Дети дока-доски.
pub async fn board_children(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    board: &Board,
) -> Result<Vec<BoardChild>> {
    let resp = client
        .call_tool(
            "list_children",
            json!({"workspaceId": cfg.workspace, "docId": board.doc_id}),
        )
        .await
        .context("list_children")?;
    Ok(parse_children(&resp))
}

/// Дети дока-доски через контекст задач.
async fn board_children_cx(cx: &mut TaskCx<'_>) -> Result<Vec<BoardChild>> {
    board_children(cx.client, cx.cfg, cx.board).await
}

/// Точное сравнение тайтлов: trim, без фолдинга регистра.
fn same_title(a: &str, b: &str) -> bool {
    a.trim() == b.trim()
}

/// Свободные (незанятые, живые, с точным тайтлом) дети — кандидаты в сироты.
/// Ребёнок, на который линкована хоть одна строка, исключается: это живая
/// задача, чей тайтл строки разошёлся с тайтлом тела — достройка затёрла бы её.
fn free_candidates<'a>(
    children: &'a [BoardChild],
    rows: &[TaskRow],
    title: &str,
) -> Vec<&'a BoardChild> {
    let occupied: Vec<&str> = rows.iter().map(|r| r.body_doc.as_str()).collect();
    children
        .iter()
        .filter(|c| !c.in_trash && same_title(&c.title, title))
        .filter(|c| !occupied.contains(&c.doc_id.as_str()))
        .collect()
}

/// Резолв существующего тела: строки доски по тайтлу ПЕРВЕЙ детей
/// (семантика юзера сформулирована на строках; дети — механизм поиска сирот).
pub async fn resolve_existing_body(cx: &mut TaskCx<'_>, title: &str) -> Result<ExistingBody> {
    let rows = read_rows(cx.client, cx.cfg, cx.board, cx.root).await?;
    let titled: Vec<&TaskRow> = rows
        .iter()
        .filter(|r| same_title(&r.title, title))
        .collect();
    if let Some(first) = titled.first() {
        return Ok(ExistingBody::Exists {
            row: (*first).clone(),
            row_count: titled.len(),
        });
    }
    let children = board_children_cx(cx).await?;
    let free = free_candidates(&children, &rows, title);
    match free.len() {
        0 => Ok(ExistingBody::None),
        1 => Ok(ExistingBody::Orphan {
            doc_id: free[0].doc_id.clone(),
        }),
        _ => Ok(ExistingBody::Ambiguous {
            doc_ids: free.iter().map(|c| c.doc_id.clone()).collect(),
        }),
    }
}

/// Единственный префикс-матч детей по docId (путь сироты task rm / cron rm).
pub fn child_by_prefix<'a>(children: &'a [BoardChild], id: &str) -> Result<&'a BoardChild> {
    let needle = id.trim_start_matches('-');
    let hits: Vec<&BoardChild> = children
        .iter()
        .filter(|c| c.doc_id.trim_start_matches('-').starts_with(needle))
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => bail!("док «{id}» среди детей доски не найден"),
        n => bail!(
            "«{id}» неоднозначно ({n} детей): {}",
            hits.iter()
                .map(|h| h.doc_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::row;
    use serde_json::json;

    #[test]
    fn parse_children_maps_fields_and_trims_titles() {
        let resp = json!({"children": [
            {"docId": "a1", "title": "  Задача  ", "inTrash": false},
            {"docId": "a2", "title": "Мусор", "inTrash": true}
        ], "count": 2});
        let kids = parse_children(&resp);
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].doc_id, "a1");
        assert_eq!(kids[0].title, "Задача");
        assert!(!kids[0].in_trash);
        assert!(kids[1].in_trash);
    }

    #[test]
    fn parse_children_tolerates_garbage() {
        assert!(parse_children(&json!({})).is_empty());
        assert!(parse_children(&json!({"children": [{"title": "no id"}]})).is_empty());
    }

    #[test]
    fn free_candidates_drop_trash_occupied_and_foreign_titles() {
        let kids = vec![
            BoardChild {
                doc_id: "orphan".into(),
                title: "T".into(),
                in_trash: false,
            },
            BoardChild {
                doc_id: "busy".into(),
                title: "T".into(),
                in_trash: false,
            },
            BoardChild {
                doc_id: "dead".into(),
                title: "T".into(),
                in_trash: true,
            },
            BoardChild {
                doc_id: "other".into(),
                title: "Другое".into(),
                in_trash: false,
            },
        ];
        let rows = vec![row("r1", "busy", "Новое")];
        let free = free_candidates(&kids, &rows, "T");
        assert_eq!(free.len(), 1);
        assert_eq!(free[0].doc_id, "orphan");
    }

    #[test]
    fn child_by_prefix_resolves_unique_and_strips_dashes() {
        let kids = vec![
            BoardChild {
                doc_id: "abc123".into(),
                title: "T".into(),
                in_trash: false,
            },
            BoardChild {
                doc_id: "def456".into(),
                title: "D".into(),
                in_trash: false,
            },
        ];
        assert_eq!(child_by_prefix(&kids, "abc").unwrap().doc_id, "abc123");
        assert_eq!(child_by_prefix(&kids, "-abc").unwrap().doc_id, "abc123");
    }

    #[test]
    fn child_by_prefix_errors_on_none_and_ambiguous() {
        let kids = vec![
            BoardChild {
                doc_id: "aa1".into(),
                title: "T".into(),
                in_trash: false,
            },
            BoardChild {
                doc_id: "aa2".into(),
                title: "D".into(),
                in_trash: false,
            },
        ];
        assert!(child_by_prefix(&kids, "zz").is_err());
        let err = child_by_prefix(&kids, "aa").unwrap_err().to_string();
        assert!(err.contains("неоднозначно"), "got: {err}");
        assert!(err.contains("aa1") && err.contains("aa2"));
    }
}
