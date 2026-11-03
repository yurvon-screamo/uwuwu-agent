//! Тесты политики заголовков: плейсхолдер вместо сети на путях показа,
//! строгий require_row_title для функционального docTitle reply.

#[cfg(test)]
mod tests {
    use crate::affine_client::AffineConfig;
    use crate::affine_gql::AffineUi;
    use crate::affine_tasks::TaskRow;
    use crate::cmd::{display_title, require_row_title};
    use serde_json::json;

    fn row(title: &str, body_doc: &str) -> TaskRow {
        TaskRow {
            row_id: "r1".into(),
            title: title.into(),
            body_doc: body_doc.into(),
            status: "Todo".into(),
            ..Default::default()
        }
    }

    fn cfg() -> AffineConfig {
        AffineConfig {
            url: String::new(),
            token: String::new(),
            workspace: "ws-test".into(),
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

    /// Строгий путь: тайтл уже есть — Ok без единого сетевого вызова
    /// сверх sign-in (дозаголовок не нужен).
    #[test]
    fn require_row_title_passes_filled_title_without_extra_calls() {
        let mock = crate::mock_ui::MockUi::start(vec![crate::mock_ui::UiStep::ok("{}".into())]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let mut r = row("Уже есть", "doc-1");

            let title = require_row_title(&ui, &cfg(), &mut r)
                .await
                .expect("тайтл на месте");

            assert_eq!(title, "Уже есть");
        });
        assert_eq!(mock.request_count(), 1, "только sign-in, дозаголовков нет");
    }

    /// Строгий путь: пустой тайтл дозаголоввается батчем через mock UI.
    #[test]
    fn require_row_title_resolves_empty_title_via_batch() {
        let batch = json!({
            "data": {"workspace": {"d0": {"id": "doc-1", "title": "Задача про таймауты"}}}
        })
        .to_string();
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::ok(batch),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let mut r = row("", "doc-1");

            let title = require_row_title(&ui, &cfg(), &mut r)
                .await
                .expect("дозаголовок");

            assert_eq!(title, "Задача про таймауты");
            assert_eq!(r.title, "Задача про таймауты");
        });
        assert_eq!(mock.request_count(), 2, "sign-in + батч");
    }

    /// Строгий путь: дозаголовок не удался (body_doc пуст) — честная ошибка,
    /// пустой docTitle недопустим.
    #[test]
    fn require_row_title_fails_when_unresolvable() {
        let mock = crate::mock_ui::MockUi::start(vec![crate::mock_ui::UiStep::ok("{}".into())]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let mut r = row("", "");

            let err = require_row_title(&ui, &cfg(), &mut r).await;

            assert!(err.is_err());
        });
    }

    /// Строгий путь: батч вернул null-тайтл (тело недоступно) — ошибка.
    #[test]
    fn require_row_title_fails_on_null_title() {
        let batch = json!({
            "data": {"workspace": {"d0": {"id": "doc-1", "title": null}}}
        })
        .to_string();
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::ok(batch),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let mut r = row("", "doc-1");

            let err = require_row_title(&ui, &cfg(), &mut r).await;

            assert!(err
                .map_err(|e| e.to_string())
                .err()
                .is_some_and(|msg| msg.contains("docTitle")));
        });
    }
}
