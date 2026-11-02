//! Точечная правка тела задачи: замена содержимого одной секции
//! по markdown-заголовку — без `--replace` (он сносит аттачи) и без
//! ручной блок-хирургии. Секция = заголовочный блок + контент до
//! следующего заголовка того же или старшего уровня; заголовочный блок
//! сохраняется, аттачи/эмбеды вне секции не трогаются.

use std::ops::Range;

use anyhow::{bail, Context as _, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};

/// Блок-потомок note в порядке документа — всё, что нужно поиску секции.
pub struct NoteBlock {
    pub id: String,
    pub kind: String,
    pub text: String,
}

/// Уровень заголовка: "h1".."h6" → 1..=6, остальное — не заголовок.
fn heading_level(kind: &str) -> Option<u8> {
    match kind.strip_prefix('h').and_then(|n| n.parse::<u8>().ok()) {
        Some(level @ 1..=6) => Some(level),
        _ => None,
    }
}

/// Нормализация заголовка к сравнению: `## Решение` == `Решение`,
/// ведущие `#` снимаются, пробельные последовательности схлопываются.
fn normalize_heading(raw: &str) -> String {
    let without_hashes = raw.trim().trim_start_matches('#');
    let mut out = String::with_capacity(without_hashes.len());
    let mut pending_space = false;
    for ch in without_hashes.trim().chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Найти секцию: (индекс заголовка, диапазон индексов содержимого).
/// Конец содержимого — следующий заголовок уровня ≤ (вложенные `###`
/// остаются внутри секции); секция последняя — до конца note.
pub fn find_section(blocks: &[NoteBlock], heading: &str) -> Result<(usize, Range<usize>)> {
    let needle = normalize_heading(heading);
    let matches: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| heading_level(&b.kind).is_some() && normalize_heading(&b.text) == needle)
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => bail!(
            "секция «{heading}» не найдена. Заголовки тела: {}",
            list_headings(blocks)
        ),
        [only] => {
            let level = heading_level(&blocks[*only].kind)
                .expect("find_section matched a heading block");
            let end = blocks[*only + 1..]
                .iter()
                .enumerate()
                .find(|(_, b)| heading_level(&b.kind).is_some_and(|l| l <= level))
                .map(|(i, _)| *only + 1 + i)
                .unwrap_or(blocks.len());
            Ok((*only, *only + 1..end))
        }
        many => bail!(
            "секция «{heading}» неоднозначна: совпадений заголовков с таким текстом: {count} — укажи уникальный",
            count = many.len()
        ),
    }
}

/// Перечень заголовков тела для честной ошибки: «h2 Решение».
fn list_headings(blocks: &[NoteBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| heading_level(&b.kind).map(|l| format!("h{l} {}", b.text.trim())))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Прочитать потомков note в порядке документа. Потомок без данных в
/// read_doc (surface-мусор) остаётся в списке с пустыми kind/text —
/// заголовком не считается, но удаляться по диапазону может.
/// Деградировавший шлюз отвечает пустым текстовым payload — чтение
/// безопасно повторить. Запись на уровне функции не ретраится;
/// transport-ретраи call_tool остаются как есть (повтор append после
/// потери ответа может задвоить контент — унаследованный риск транспорта,
/// тот же у append_body_markdown).
async fn read_note_blocks(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
) -> Result<Vec<NoteBlock>> {
    let mut last_err = None;
    for _attempt in 0..2 {
        match read_note_blocks_once(client, cfg, body_doc).await {
            Ok(blocks) => return Ok(blocks),
            Err(e) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.expect("retry loop runs at least once"))
}

async fn read_note_blocks_once(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
) -> Result<Vec<NoteBlock>> {
    let doc = client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": body_doc}),
        )
        .await
        .context("read_doc body")?;
    let blocks = doc
        .get("blocks")
        .and_then(|b| b.as_array())
        .ok_or_else(|| anyhow::anyhow!("пустой read_doc"))?;
    let note = blocks
        .iter()
        .find(|b| b.get("flavour").and_then(|f| f.as_str()) == Some("affine:note"))
        .ok_or_else(|| anyhow::anyhow!("в теле нет note-блока"))?;
    let by_id: std::collections::HashMap<&str, &serde_json::Value> = blocks
        .iter()
        .filter_map(|b| b.get("id").and_then(|i| i.as_str()).map(|id| (id, b)))
        .collect();
    let child_ids = note
        .get("childIds")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(child_ids
        .into_iter()
        .map(|id| {
            let b = by_id.get(id).copied();
            NoteBlock {
                id: id.to_string(),
                kind: b
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                text: b
                    .and_then(|b| b.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        })
        .collect())
}

/// Заменить содержимое секции `heading` тела: блоки содержимого удаляются,
/// `md` вставляется сразу после заголовка секции (html_embed-фенсы →
/// эмбеды). Пустой md = очистка секции. Возвращает (удалено блоков,
/// вставлено диаграмм).
pub async fn replace_body_section(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    body_doc: &str,
    heading: &str,
    md: &str,
) -> Result<(usize, usize)> {
    let note_blocks = read_note_blocks(client, cfg, body_doc).await?;
    let (heading_idx, content) = find_section(&note_blocks, heading)?;
    let heading_id = note_blocks[heading_idx].id.clone();

    // Платформа без транзакций: сбой в середине серии оставляет секцию
    // частично удалённой — ошибка обязана говорить, сколько уже применено.
    for (deleted, block) in note_blocks[content.clone()].iter().enumerate() {
        let resp = client
            .call_tool(
                "delete_block",
                json!({"workspaceId": cfg.workspace, "docId": body_doc, "blockId": block.id}),
            )
            .await
            .with_context(|| {
                format!(
                    "delete_block {}: до сбоя удалено {deleted} блоков секции",
                    block.id
                )
            })?;
        if resp.get("ok").and_then(|o| o.as_bool()) == Some(false) {
            bail!(
                "delete_block {}: {} — до сбоя удалено {deleted} блоков секции",
                block.id,
                resp.get("error").and_then(|e| e.as_str()).unwrap_or("?")
            );
        }
    }

    let mut diagrams = 0usize;
    if !md.trim().is_empty() {
        let cleared = format!(
            "секция уже очищена (удалено {} блоков), новый контент НЕ вставлен",
            content.len()
        );
        let resp = client
            .call_tool(
                "append_markdown",
                json!({"workspaceId": cfg.workspace, "docId": body_doc, "markdown": md,
                       "placement": {"afterBlockId": heading_id}}),
            )
            .await
            .with_context(|| format!("append_markdown: {cleared}"))?;
        if resp.get("ok").and_then(|o| o.as_bool()) == Some(false) {
            bail!(
                "append_markdown: {} — {cleared}",
                resp.get("error").and_then(|e| e.as_str()).unwrap_or("?")
            );
        }
        diagrams = crate::affine_tasks::convert_embed_fences(client, cfg, body_doc).await?;
    }
    Ok((content.len(), diagrams))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, kind: &str, text: &str) -> NoteBlock {
        NoteBlock {
            id: id.to_string(),
            kind: kind.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn normalize_heading_strips_hashes_and_collapses_whitespace() {
        assert_eq!(normalize_heading("## Решение"), "Решение");
        assert_eq!(
            normalize_heading("  ###   Раздел   про  всё  "),
            "Раздел про всё"
        );
        assert_eq!(normalize_heading("Решение"), "Решение");
        assert_eq!(normalize_heading(""), "");
    }

    #[test]
    fn find_section_spans_content_until_next_same_level_heading() {
        // Arrange
        let blocks = vec![
            block("t", "h1", "Заголовок"),
            block("h2a", "h2", "Постановка"),
            block("p1", "text", "текст постановки"),
            block("h2b", "h2", "Решение"),
            block("p2", "text", "текст решения"),
        ];

        // Act
        let (heading_idx, content) = find_section(&blocks, "Постановка").unwrap();

        // Assert
        assert_eq!(blocks[heading_idx].id, "h2a");
        assert_eq!(content, 2..3);
    }

    #[test]
    fn find_section_accepts_hash_prefixed_heading_argument() {
        // Arrange
        let blocks = vec![block("h2", "h2", "Решение"), block("p", "text", "текст")];

        // Act
        let (_, content) = find_section(&blocks, "## Решение").unwrap();

        // Assert
        assert_eq!(content, 1..2);
    }

    #[test]
    fn find_section_keeps_deeper_headings_inside_the_section() {
        // Arrange: вложенный h3 принадлежит секции h2, соседний h2 — нет
        let blocks = vec![
            block("h2a", "h2", "Решение"),
            block("p1", "text", "до"),
            block("h3", "h3", "Детали"),
            block("p2", "text", "внутри"),
            block("h2b", "h2", "Проверка"),
            block("p3", "text", "чужое"),
        ];

        // Act
        let (_, content) = find_section(&blocks, "Решение").unwrap();

        // Assert
        assert_eq!(content, 1..4);
    }

    #[test]
    fn find_section_last_section_extends_to_end_of_note() {
        // Arrange
        let blocks = vec![
            block("h2a", "h2", "Решение"),
            block("h2b", "h2", "Проверка"),
            block("p1", "text", "тесты"),
            block("li", "bulleted", "cargo test"),
        ];

        // Act
        let (_, content) = find_section(&blocks, "Проверка").unwrap();

        // Assert
        assert_eq!(content, 2..4);
    }

    #[test]
    fn find_section_missing_heading_error_lists_available_headings() {
        // Arrange
        let blocks = vec![block("h2", "h2", "Решение"), block("p", "text", "текст")];

        // Act
        let err = find_section(&blocks, "Юзкейсы").unwrap_err().to_string();

        // Assert
        assert!(err.contains("не найдена"), "{err}");
        assert!(err.contains("h2 Решение"), "{err}");
    }

    #[test]
    fn find_section_duplicate_heading_is_ambiguous_error() {
        // Arrange
        let blocks = vec![block("h2a", "h2", "Заметки"), block("h2b", "h2", "Заметки")];

        // Act
        let err = find_section(&blocks, "Заметки").unwrap_err().to_string();

        // Assert
        assert!(err.contains("неоднозначна"), "{err}");
        assert!(err.contains("с таким текстом: 2"), "{err}");
    }

    #[test]
    fn plain_paragraph_with_heading_text_is_not_a_section() {
        // Arrange: абзац «Решение» — не заголовок, секцией не считается
        let blocks = vec![block("p", "text", "Решение"), block("h2", "h2", "Проверка")];

        // Act
        let err = find_section(&blocks, "Решение").unwrap_err().to_string();

        // Assert
        assert!(err.contains("не найдена"), "{err}");
    }

    #[test]
    fn heading_level_parses_h1_to_h6_only() {
        assert_eq!(heading_level("h1"), Some(1));
        assert_eq!(heading_level("h6"), Some(6));
        assert_eq!(heading_level("h7"), None);
        assert_eq!(heading_level("h0"), None);
        assert_eq!(heading_level("text"), None);
        assert_eq!(heading_level("bulleted"), None);
        assert_eq!(heading_level("numbered"), None);
    }
}
