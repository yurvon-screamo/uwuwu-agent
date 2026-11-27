//! Vanilla egui 0.37 layout primitives for a narrow panel. Compiled in
//! scratch against egui/eframe 0.37 and egui_extras 0.37 on 2026-11-27.
//! Keep breakpoints in one module. Measure them in logical points.

use egui::{Response, Ui};
use egui_extras::{Size, StripBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeClass {
    Compact,
    Normal,
    Wide,
}

impl SizeClass {
    pub fn from_width(points: f32) -> Self {
        if points < 300.0 {
            Self::Compact
        } else if points < 600.0 {
            Self::Normal
        } else {
            Self::Wide
        }
    }

    pub fn min_chat_width(self) -> f32 {
        match self {
            Self::Compact => 160.0,
            Self::Normal => 260.0,
            Self::Wide => 380.0,
        }
    }
}

/// A title that truncates and a trailing button that stays inside the panel.
/// `shrink_right` limits the left side; `truncate` keeps the title readable
/// from its start.
pub fn header(ui: &mut Ui, title: &str, action: &str) -> Response {
    let (_, action_response) = egui::Sides::new()
        .shrink_right()
        .truncate()
        .show(
            ui,
            |ui| {
                ui.label(title);
            },
            |ui| ui.button(action),
        );
    action_response
}

/// A fixed label column beside a flexible, clipped column. `at_least`
/// protects the flexible column when the panel narrows.
pub fn picker(ui: &mut Ui, caption: &str, rows: usize) {
    // Measure before the builder takes the mutable borrow.
    let min_width = SizeClass::from_width(ui.available_width()).min_chat_width();
    StripBuilder::new(ui)
        .clip(true)
        .size(Size::exact(96.0))
        .size(Size::remainder().at_least(min_width))
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                ui.label(caption);
            });
            strip.cell(|ui| {
                for row in 0..rows {
                    let _ = ui.button(format!("Row {row}"));
                }
            });
        });
}
