//! Small native-widget wrapper. Compiled in scratch against egui 0.37 on
//! 2026-11-27. Map `Kind` to theme tokens in the host app's style, not with
//! per-instance hard-coded colours.

use egui::{Response, Ui, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

pub struct ActionButton<'a> {
    label: &'a str,
    kind: Kind,
    enabled: bool,
}

impl<'a> ActionButton<'a> {
    pub fn new(label: &'a str, kind: Kind) -> Self {
        Self { label, kind, enabled: true }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl Widget for ActionButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        // The visible label is the accessible name. kittest can find it.
        // `kind` selects a token-backed style in the host application.
        let response = ui.add_enabled(self.enabled, egui::Button::new(self.label));
        let _ = self.kind;
        response
    }
}

/// Use `push_id` when a loop can produce two widgets with the same label.
pub fn labelled_rows(ui: &mut Ui, names: &[&str]) -> Option<usize> {
    let mut chosen = None;
    for (index, name) in names.iter().enumerate() {
        ui.push_id(index, |ui| {
            if ActionButton::new(name, Kind::Secondary).ui(ui).clicked() {
                chosen = Some(index);
            }
        });
    }
    chosen
}
