//! Тесты политики заголовков: плейсхолдер вместо сети на путях показа.

#[cfg(test)]
mod tests {
    use crate::affine_tasks::TaskRow;
    use crate::cmd::display_title;

    fn row(title: &str, body_doc: &str) -> TaskRow {
        TaskRow {
            row_id: "r1".into(),
            title: title.into(),
            body_doc: body_doc.into(),
            status: "Todo".into(),
            ..Default::default()
        }
    }

    #[test]
    fn resolved_title_passes_through() {
        assert_eq!(
            display_title(&row("Задача про таймауты", "doc1")),
            "Задача про таймауты"
        );
    }

    #[test]
    fn empty_title_shows_body_doc_placeholder() {
        assert_eq!(display_title(&row("", "5HuB9qRLOF")), "(тело 5HuB9qRLOF)");
    }

    #[test]
    fn empty_title_without_body_shows_no_body_marker() {
        assert_eq!(display_title(&row("", "")), "(без тела)");
    }
}
