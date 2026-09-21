//! Colors and visual constants for the dark-first (default), Discord/Mumble-
//! style palette; follows the configured theme (`system`/`light`/`dark`).

use std::collections::BTreeMap;

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// The single accent color, also used by the tray icon and the Wayland
/// overlay's speaking-tile border (`tray.rs`, `overlay.rs` — both hardcode
/// this exact RGB since neither can import this module; keep in sync if it
/// ever changes).
pub const ACCENT: Color32 = Color32::from_rgb(0x2e, 0x9b, 0xff);

/// Status dot: connected.
pub const CONNECTED: Color32 = Color32::from_rgb(0x4a, 0x9e, 0x5c);
/// Status dot: connecting / reconnecting.
pub const CONNECTING: Color32 = Color32::from_rgb(0xd9, 0xa4, 0x3d);
/// Status dot / banner: error or fatal.
pub const ERROR: Color32 = Color32::from_rgb(0xd9, 0x53, 0x4f);
/// Speaking indicator bar and PTT-active fill; == [`ACCENT`].
pub const SPEAKING: Color32 = ACCENT;
/// Muted / deafened icon.
pub const MUTED: Color32 = Color32::from_rgb(0xd9, 0x53, 0x4f);
/// Level meter fill.
pub const LEVEL: Color32 = Color32::from_rgb(0x5c, 0xb3, 0x6b);
/// Level meter above the VAD threshold (currently transmitting).
pub const LEVEL_HOT: Color32 = Color32::from_rgb(0xe0, 0x9a, 0x3e);

/// Applies the configured theme (`system`, `light`, `dark`) plus the app's
/// own palette, spacing and type scale. Runs on every `Command::UpdateSettings`
/// (whenever the theme setting is touched), so it must stay cheap and must
/// never rebuild the font atlas — see `fonts::install`, called once instead.
pub fn apply(ctx: &egui::Context, theme: &str) {
    match theme {
        "light" => ctx.set_theme(egui::Theme::Light),
        "dark" => ctx.set_theme(egui::Theme::Dark),
        _ => ctx.set_theme(egui::ThemePreference::System),
    }
    ctx.set_visuals_of(egui::Theme::Dark, dark_visuals());
    ctx.set_visuals_of(egui::Theme::Light, light_visuals());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        style.text_styles = text_styles();
    });
}

/// Body/heading/button type scale, sized up from egui's stock defaults
/// (Small 9 / Body 13 / Button 13 / Heading 18) and routed through Inter.
fn text_styles() -> BTreeMap<TextStyle, FontId> {
    let semibold = super::fonts::semibold();
    BTreeMap::from([
        (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(15.0, semibold.clone())),
        (TextStyle::Heading, FontId::new(20.0, semibold)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ])
}

fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(0x12, 0x13, 0x17);
    v.window_fill = v.panel_fill;
    v.extreme_bg_color = Color32::from_rgb(0x0a, 0x0a, 0x0c);
    v.faint_bg_color = Color32::from_rgb(0x1a, 0x1b, 0x20);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.5);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(45));

    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    // Differentiate the "track" role (bg_fill: checkbox/slider background)
    // from the "button chrome" role (weak_bg_fill) — stock egui keeps these
    // equal in every state.
    v.widgets.inactive.weak_bg_fill = Color32::from_gray(38);
    v.widgets.inactive.bg_fill = Color32::from_gray(24);
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(48);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.85);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v
}

fn light_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.active.weak_bg_fill = ACCENT;
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v
}

/// Maps dBFS to a 0–1 meter position (−60 dB … 0 dB).
pub fn meter_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}
