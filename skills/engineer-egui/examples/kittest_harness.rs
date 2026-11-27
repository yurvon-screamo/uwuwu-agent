//! Minimal egui_kittest 0.37 harness. Compiled in scratch against egui_kittest
//! 0.37 with default features off on 2026-11-27. The visible label is the
//! AccessKit name, so the test selects a widget without an implementation id.

use egui_kittest::{kittest::Queryable, Harness};

pub fn checkbox_can_be_clicked_by_label() {
    let mut harness = Harness::new_ui_state(
        |ui, enabled: &mut bool| {
            ui.checkbox(enabled, "Enable session");
        },
        false,
    );

    harness.get_by_label("Enable session").click();
    harness.run();

    assert!(*harness.state());
}

/// Check absence without panicking. `get_by_*` panics on a missing widget;
/// `query_all_by_label_contains` returns an iterator.
pub fn absent_widget_is_not_panicking(harness: &Harness<bool>) -> bool {
    harness.query_all_by_label_contains("Deleted session").next().is_none()
}
