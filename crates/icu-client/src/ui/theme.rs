//! Colors and visual constants; follows the system light/dark theme.

use egui::Color32;

/// Status dot colors.
pub const CONNECTED: Color32 = Color32::from_rgb(0x4c, 0xaf, 0x50);
/// Connecting.
pub const CONNECTING: Color32 = Color32::from_rgb(0xff, 0xc1, 0x07);
/// Error / fatal.
pub const ERROR: Color32 = Color32::from_rgb(0xf4, 0x43, 0x36);
/// Speaking bar.
pub const SPEAKING: Color32 = Color32::from_rgb(0x2e, 0x9b, 0xff);
/// Muted icon.
pub const MUTED: Color32 = Color32::from_rgb(0xf4, 0x43, 0x36);
/// Level meter fill.
pub const LEVEL: Color32 = Color32::from_rgb(0x66, 0xbb, 0x6a);
/// Level meter above the VAD threshold.
pub const LEVEL_HOT: Color32 = Color32::from_rgb(0xff, 0x98, 0x00);

/// Applies the configured theme (`system`, `light`, `dark`).
pub fn apply(ctx: &egui::Context, theme: &str) {
    match theme {
        "light" => ctx.set_theme(egui::Theme::Light),
        "dark" => ctx.set_theme(egui::Theme::Dark),
        _ => ctx.set_theme(egui::ThemePreference::System),
    }
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.visuals.window_corner_radius = egui::CornerRadius::same(8);
    });
}

/// Maps dBFS to a 0–1 meter position (−60 dB … 0 dB).
pub fn meter_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}
