//! Colors and visual constants: a calm near-black dark palette and a clean
//! light one; follows the configured theme (`system` by default).
//!
//! Every color role differs between the dark and light theme (2026 design
//! pass), so almost nothing here is a flat constant: [`tokens`] returns the
//! full theme-dependent palette, keyed off `Visuals::dark_mode`. It's cheap
//! (a handful of `Color32::from_rgb` calls and one branch) and safe to call
//! once per widget/render function at 60 fps. Only spacing, radii and the
//! type scale stay flat, since they don't vary by theme.

use std::collections::BTreeMap;

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// The full theme-dependent color palette. Call [`tokens`] once per
/// widget/render function — cheap, no allocation.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    /// Accent: buttons, links, active states, speaking indicator, PTT-active fill.
    pub accent: Color32,
    /// Text/icon color atop [`Self::accent`] (replaces the old hardcoded white,
    /// which only reached ~2.9:1 contrast on the dark accent).
    pub on_accent: Color32,
    /// ~10% accent surface: speaking-row background, active-button tint.
    pub accent_tint: Color32,
    /// ~16% accent surface: logo badge background.
    pub accent_tint_2: Color32,
    /// Status dot / text: connected.
    pub connected: Color32,
    /// Status dot / text: connecting / reconnecting.
    pub connecting: Color32,
    /// Status dot / banner / mute-deafen fills and icons: error or fatal.
    pub error: Color32,
    /// Error banner tint surface.
    pub error_tint: Color32,
    /// Warning banner tint surface.
    pub warning_tint: Color32,
    /// Level meter fill.
    pub level: Color32,
    /// Muted/secondary text, still body-sized (e.g. server host, help text).
    pub muted_text: Color32,
    /// Dim/tertiary text (min/max labels, chip captions).
    pub dim_text: Color32,
    /// Translucent wash laid over any surface under the pointer: light on
    /// dark, dark on light, so hover is visible in both themes.
    pub hover_overlay: Color32,
    /// A surface one step above `faint_bg_color`: round toggles, the speak
    /// surface's resting fill.
    pub surface_hi: Color32,
}

/// Corner-radius scale. Four steps, nested smallest-inside-largest: a chip
/// inside a control inside a row inside a card. Every rounded thing in the
/// app picks one of these, so no call site has to invent a radius.
pub const R_CHIP: u8 = 5;
/// Buttons, segmented tracks, combo boxes.
pub const R_CTRL: u8 = 8;
/// Participant rows, banners, the big speak/control buttons.
pub const R_ROW: u8 = 9;
/// `widgets::group_card` and the settings rail.
pub const R_CARD: u8 = 8;

/// Returns the palette for the current theme (`visuals.dark_mode`).
pub fn tokens(visuals: &Visuals) -> Tokens {
    if visuals.dark_mode {
        Tokens {
            accent: Color32::from_rgb(0x5a, 0xd0, 0x8f),
            on_accent: Color32::from_rgb(0x08, 0x19, 0x0f),
            accent_tint: Color32::from_rgb(0x14, 0x23, 0x1b),
            accent_tint_2: Color32::from_rgb(0x2c, 0x3a, 0x32),
            connected: Color32::from_rgb(0x5a, 0xd0, 0x8f),
            connecting: Color32::from_rgb(0xe3, 0xa8, 0x3f),
            error: Color32::from_rgb(0xf0, 0x6a, 0x72),
            error_tint: Color32::from_rgb(0x2a, 0x16, 0x19),
            warning_tint: Color32::from_rgb(0x22, 0x1d, 0x12),
            level: Color32::from_rgb(0x5a, 0xd0, 0x8f),
            muted_text: Color32::from_rgb(0xa3, 0xa4, 0xaa),
            dim_text: Color32::from_rgb(0x8a, 0x8b, 0x92),
            hover_overlay: Color32::from_white_alpha(14),
            surface_hi: Color32::from_rgb(0x20, 0x21, 0x25),
        }
    } else {
        Tokens {
            accent: Color32::from_rgb(0x13, 0x7a, 0x43),
            on_accent: Color32::WHITE,
            accent_tint: Color32::from_rgb(0xe5, 0xf2, 0xea),
            accent_tint_2: Color32::from_rgb(0xcf, 0xe6, 0xd8),
            connected: Color32::from_rgb(0x13, 0x7a, 0x43),
            connecting: Color32::from_rgb(0xb7, 0x74, 0x00),
            error: Color32::from_rgb(0xc2, 0x34, 0x3b),
            error_tint: Color32::from_rgb(0xfb, 0xeb, 0xeb),
            warning_tint: Color32::from_rgb(0xfb, 0xf3, 0xe2),
            level: Color32::from_rgb(0x13, 0x7a, 0x43),
            muted_text: Color32::from_rgb(0x52, 0x53, 0x5a),
            dim_text: Color32::from_rgb(0x69, 0x6a, 0x71),
            hover_overlay: Color32::from_black_alpha(12),
            surface_hi: Color32::from_rgb(0xec, 0xec, 0xea),
        }
    }
}

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
        style.spacing.item_spacing = egui::vec2(12.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.combo_height = 320.0;
        style.text_styles = text_styles();
        // Every scrollbar in the app is an overlay over a card or a list:
        // floating keeps it from stealing 12px of content width, and the
        // thin handle keeps it quiet until it is touched.
        style.spacing.scroll = egui::style::ScrollStyle::thin();
    });
}

/// Body/heading/button type scale, routed through Inter.
fn text_styles() -> BTreeMap<TextStyle, FontId> {
    let semibold = super::fonts::semibold();
    BTreeMap::from([
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, semibold.clone())),
        (TextStyle::Heading, FontId::new(17.0, semibold)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ])
}

/// A semibold 14.5px `RichText` for the "Strong" weight egui has no
/// dedicated `TextStyle` for (participant names, banner titles).
pub fn strong_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).font(FontId::new(14.5, super::fonts::semibold()))
}

/// A caps `RichText` for `widgets::group_card` section titles. egui has no
/// `letter_spacing`, so this is Small + semibold + uppercased text only.
pub fn caps_text(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .text_style(TextStyle::Small)
        .font(FontId::new(11.0, super::fonts::semibold()))
}

fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    let t = tokens(&v);
    v.panel_fill = Color32::from_rgb(0x10, 0x11, 0x13);
    v.window_fill = v.panel_fill;
    v.extreme_bg_color = Color32::from_rgb(0x16, 0x17, 0x1a);
    v.faint_bg_color = Color32::from_rgb(0x1b, 0x1c, 0x20);
    v.override_text_color = Some(Color32::from_rgb(0xec, 0xec, 0xea));
    v.hyperlink_color = t.accent;
    v.selection.bg_fill = t.accent.gamma_multiply(0.5);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.window_corner_radius = CornerRadius::same(R_ROW);
    v.menu_corner_radius = CornerRadius::same(R_CTRL);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2b, 0x30));

    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(R_CTRL);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x22, 0x23, 0x27));
    // Differentiate the "track" role (bg_fill: checkbox/slider background)
    // from the "button chrome" role (weak_bg_fill) — stock egui keeps these
    // equal in every state.
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2b, 0x30));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x20, 0x21, 0x25);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x16, 0x17, 0x1a);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2a, 0x2b, 0x30);
    v.widgets.active.weak_bg_fill = t.accent.gamma_multiply(0.85);
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.on_accent);
    v
}

fn light_visuals() -> Visuals {
    let mut v = Visuals::light();
    let t = tokens(&v);
    v.panel_fill = Color32::from_rgb(0xf6, 0xf6, 0xf4);
    v.window_fill = v.panel_fill;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(0xff, 0xff, 0xff);
    v.override_text_color = Some(Color32::from_rgb(0x18, 0x18, 0x1b));
    v.hyperlink_color = t.accent;
    v.selection.bg_fill = t.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.window_corner_radius = CornerRadius::same(R_ROW);
    v.menu_corner_radius = CornerRadius::same(R_CTRL);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(0xdf, 0xdf, 0xdb));
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(R_CTRL);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xe9, 0xe9, 0xe5));
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xdf, 0xdf, 0xdb));
    v.widgets.active.weak_bg_fill = t.accent;
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.on_accent);
    v
}

/// Maps dBFS to a 0–1 meter position (−60 dB … 0 dB).
pub fn meter_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sRGB relative luminance (WCAG 2.x definition).
    fn relative_luminance(c: Color32) -> f64 {
        let lin = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    /// WCAG contrast ratio between two colors, order-independent.
    fn contrast_ratio(a: Color32, b: Color32) -> f64 {
        let (l1, l2) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Named so clippy's `type_complexity` has something to point at.
    type Theme = (&'static str, fn() -> Visuals);

    const THEMES: [Theme; 2] = [("dark", dark_visuals), ("light", light_visuals)];

    #[test]
    fn text_on_panel_meets_wcag_aa() {
        for (name, make) in THEMES {
            let v = make();
            let text = v.override_text_color.expect("text color set");
            let ratio = contrast_ratio(text, v.panel_fill);
            assert!(ratio >= 4.5, "{name}: text/panel contrast {ratio:.2} < 4.5");
        }
    }

    #[test]
    fn muted_text_meets_wcag_aa() {
        for (name, make) in THEMES {
            let v = make();
            let t = tokens(&v);
            let ratio = contrast_ratio(t.muted_text, v.panel_fill);
            assert!(
                ratio >= 4.5,
                "{name}: muted_text/panel contrast {ratio:.2} < 4.5"
            );
        }
    }

    #[test]
    fn dim_text_meets_floor() {
        for (name, make) in THEMES {
            let v = make();
            let t = tokens(&v);
            let ratio = contrast_ratio(t.dim_text, v.panel_fill);
            assert!(
                ratio >= 3.0,
                "{name}: dim_text/panel contrast {ratio:.2} < 3.0"
            );
        }
    }

    #[test]
    fn on_accent_meets_wcag_aa() {
        for (name, make) in THEMES {
            let v = make();
            let t = tokens(&v);
            let ratio = contrast_ratio(t.on_accent, t.accent);
            assert!(
                ratio >= 4.5,
                "{name}: on_accent/accent contrast {ratio:.2} < 4.5"
            );
        }
    }

    #[test]
    fn status_colors_meet_non_text_floor() {
        for (name, make) in THEMES {
            let v = make();
            let t = tokens(&v);
            for (label, c) in [
                ("connected", t.connected),
                ("connecting", t.connecting),
                ("error", t.error),
            ] {
                let ratio = contrast_ratio(c, v.panel_fill);
                assert!(
                    ratio >= 3.0,
                    "{name}: {label}/panel contrast {ratio:.2} < 3.0"
                );
            }
        }
    }
}
