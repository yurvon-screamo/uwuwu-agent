//! Общие фикстуры юнит-тестов крейта (только под cfg(test)).

use crate::affine_tasks::TaskRow;

/// Строка канбана для тестов резолвов/патчей.
pub(crate) fn row(row_id: &str, body: &str, title: &str) -> TaskRow {
    TaskRow {
        row_id: row_id.into(),
        body_doc: body.into(),
        title: title.into(),
        ..Default::default()
    }
}
