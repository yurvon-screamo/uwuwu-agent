//! Запись тела задачи: replace/append в живой док, диаграммные операции,
//! снапшот --base.

use crate::cmd::sy_dispatch::body_comments::{carry_over_comments_section, require_fresh_snapshot};
use crate::cmd::sy_dispatch::body_sections::{report_section_edit, replace_md_section};
use crate::cmd::sy_dispatch::guards::{looks_like_inline_text, not_a_path_error};
use crate::html_embed;
use crate::markdown;
use crate::siyuan::find_marker_location as sy_find_marker_location;
use crate::siyuan::SiYuan;
use anyhow::{Context, Result};

/// Параметры rewrite-пути task body (replace / replace-section).
pub(super) struct BodyRewrite<'a> {
    /// Секционная правка: existing-база сохраняется механикой replace_md_section,
    /// гард свежести не несёт; None — полный replace целиком.
    pub(super) section: Option<&'a str>,
    /// Снапшот свежести --base (текст файла); None + !force = отказ.
    pub(super) snapshot: Option<&'a str>,
    /// Осознанный overwrite без сверки.
    pub(super) force: bool,
}

/// Запись тела in-place: docId сохраняется, привязка строки не рвётся.
/// rewrite=None — append: block-хирургия именованных html_embed-фенсов
/// (update/insert/delete по маркерам) + один appendBlock остатка; Some —
/// экспорт по id, strip frontmatter (иначе он ляжет литеральным блоком),
/// фенсы инлайнятся парами «маркер + html», пересборка контента.
/// Отказ гарда — Err до overwrite: экспорт read-only, ядро не тронуто.
/// Валидация ввода (дубль имени, пустая строка в html) — Err до любых мутаций.
pub(super) async fn write_body_in_place(
    sy: &SiYuan,
    doc_id: &str,
    doc_title: &str,
    rewrite: Option<BodyRewrite<'_>>,
    content: &str,
) -> Result<()> {
    let Some(rewrite) = rewrite else {
        return append_with_diagrams(sy, doc_id, content).await;
    };
    let inline = html_embed::parse_input(content)?.inline_md();
    // титульный H1 из контента (снапшот-пасты) — дубль к док-заголовку:
    // стрип до сборки и записи, инвариант «не более одного H1 = док-тайтл»
    let inline = markdown::strip_title_h1(&inline, doc_title);
    let existing_raw = sy.export_md_by_id(doc_id).await.unwrap_or_default();
    let existing = markdown::strip_export_wrapper(&existing_raw, doc_title);
    let new_md = match rewrite.section {
        Some(section) => {
            let (md, edit) = replace_md_section(&existing, section, &inline)?;
            report_section_edit(section, edit);
            md
        }
        None => {
            if !rewrite.force {
                require_fresh_snapshot(doc_title, &existing, rewrite.snapshot)?;
            }
            carry_over_comments_section(doc_title, &existing, &inline)?
        }
    };
    sy.overwrite_doc_md(doc_id, doc_title, &new_md).await
}

/// append-путь с фенсами: именованные — block-хирургия в доке (адресация
/// по маркер-абзацам из .sy-порядка детей), потом остаток одним appendBlock.
/// Живые html-блоки вне фенсов не трогаются (экспорт ядра отдаёт их сырым
/// HTML, append остатка их не пересобирает).
async fn append_with_diagrams(sy: &SiYuan, doc_id: &str, content: &str) -> Result<()> {
    let parsed = html_embed::parse_input(content)?;
    for fence in parsed.named_fences() {
        let children = sy.doc_children(doc_id).await?;
        let location =
            sy_find_marker_location(&children, html_embed::DIAGRAM_MARKER_PREFIX, &fence.name);
        let op = html_embed::plan_append_op(location.as_ref(), &fence);
        apply_diagram_op(sy, doc_id, op).await?;
    }
    let residual = parsed.residual_md();
    if !residual.trim().is_empty() {
        sy.append_block_md(doc_id, &residual).await?;
    }
    Ok(())
}

async fn apply_diagram_op(sy: &SiYuan, doc_id: &str, op: html_embed::DiagramOp) -> Result<()> {
    match op {
        html_embed::DiagramOp::UpdateExisting { block_id, html } => {
            sy.update_block_md(&block_id, &html).await
        }
        html_embed::DiagramOp::DeleteExisting { block_id } => sy.delete_block(&block_id).await,
        html_embed::DiagramOp::InsertAfterMarker { marker_id, html } => {
            sy.insert_block_md_after(&marker_id, &html).await
        }
        html_embed::DiagramOp::AppendNewPair { name, html } => {
            // маркер + html одним markdown-вызовом: ядро создаёт оба блока
            // (пара подтверждена пробами ядра 3.8.10)
            let pair = format!("{}\n\n{html}", html_embed::marker_text(&name));
            sy.append_block_md(doc_id, &pair).await
        }
        html_embed::DiagramOp::Nothing => Ok(()),
    }
}

/// Снапшот --base: локальное чтение до сети (fail fast). Не-путь — понятный
/// отказ; нечитаемый путь — контекст вызова поверх честной OS-причины.
pub(super) fn read_base_snapshot(base: Option<&str>) -> Result<Option<String>> {
    let Some(path) = base else {
        return Ok(None);
    };
    if looks_like_inline_text(path) {
        return Err(not_a_path_error(
            "--base",
            path,
            "сохрани снапшот в файл и передай путь",
        ));
    }
    Ok(Some(std::fs::read_to_string(path).with_context(|| {
        format!("снапшот --base не читается: {path}")
    })?))
}
