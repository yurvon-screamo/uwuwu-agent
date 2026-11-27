//! Minimal vanilla egui 0.37 theme module. Adapted from the craft token
//! pattern; see ATTRIBUTION.md. Paste into a crate that already depends on
//! egui = "0.37". Compiled in scratch against egui 0.37 on 2026-11-27.

use egui::{Color32, Context, CornerRadius, FontId, Stroke, TextStyle, Visuals};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub bg: Color32,
    pub sunken: Color32,
    pub ink: Color32,
    pub line: Color32,
    pub accent: Color32,
}

impl Tokens {
    pub const DARK: Self = Self {
        bg: Color32::from_rgb(0x12, 0x13, 0x16),
        sunken: Color32::from_rgb(0x06, 0x06, 0x07),
        ink: Color32::from_rgb(0xf2, 0xf1, 0xee),
        line: Color32::from_rgba_unmultiplied_const(0xf2, 0xf1, 0xee, 38),
        accent: Color32::from_rgb(0x2d, 0x81, 0xff),
    };

    pub fn hairline(self) -> Stroke {
        Stroke::new(1.0, self.line)
    }
}

/// Apply once per Context. Widgets read Tokens; they never hard-code a colour.
pub fn apply(ctx: &Context, tokens: Tokens) {
    ctx.all_styles_mut(|style| {
        style.visuals = Visuals::dark();
        style.visuals.window_fill = tokens.bg;
        style.visuals.panel_fill = tokens.bg;
        style.visuals.widgets.noninteractive.bg_stroke = tokens.hairline();
        style.visuals.widgets.inactive.bg_stroke = tokens.hairline();
        style.visuals.widgets.noninteractive.corner_radius = CornerRadius::ZERO;
        style.visuals.widgets.inactive.corner_radius = CornerRadius::ZERO;
        style.visuals.hyperlink_color = tokens.accent;
        style.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
    });
}
