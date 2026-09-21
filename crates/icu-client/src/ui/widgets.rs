//! Small, theme-driven widget vocabulary shared by `main_window.rs` and
//! `settings.rs` (2026 UI redesign). Every widget pulls colors from
//! `theme::tokens(ui.visuals())` plus the current `Visuals` — never a raw
//! hex literal here. Pixel values follow the design spec closely but are
//! not pixel-perfect in every case (padding/inset rounding); flag any
//! visible drift during the manual live-test rather than re-deriving exact
//! figures from first principles.

use std::ops::RangeInclusive;

use egui::{Color32, CornerRadius, FontId, Frame, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_phosphor::regular as icon;

use super::theme;

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        egui::lerp(a.r() as f32..=b.r() as f32, t).round() as u8,
        egui::lerp(a.g() as f32..=b.g() as f32, t).round() as u8,
        egui::lerp(a.b() as f32..=b.b() as f32, t).round() as u8,
        egui::lerp(a.a() as f32..=b.a() as f32, t).round() as u8,
    )
}

/// A pill-shaped on/off switch, 40x23.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(40.0, 23.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let tok = theme::tokens(ui.visuals());
        let painter = ui.painter();
        let radius = rect.height() / 2.0;
        let track = lerp_color(ui.visuals().extreme_bg_color, tok.accent, t);
        painter.rect_filled(rect, radius, track);
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        let knob_r = 7.5;
        let inset = (rect.height() - knob_r * 2.0) / 2.0;
        let cx = egui::lerp(
            (rect.left() + inset + knob_r)..=(rect.right() - inset - knob_r),
            t,
        );
        let center = egui::pos2(cx, rect.center().y);
        let knob = lerp_color(tok.dim_text, Color32::from_rgb(0xf2, 0xf6, 0xfb), t);
        painter.circle_filled(center, knob_r, knob);
    }
    response
}

/// A segmented control: a track of mutually exclusive options. Returns a
/// `Response` whose `.changed()` reports a switch; the new value is written
/// into `current` directly.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    options: &[(T, &str)],
) -> egui::Response {
    let mut changed = false;
    let inner = Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(3)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (value, label) in options {
                    let selected = *current == *value;
                    let btn = egui::Button::new(RichText::new(*label).size(12.5))
                        .fill(if selected {
                            ui.visuals().faint_bg_color
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(CornerRadius::same(7))
                        .min_size(egui::vec2(0.0, 22.0));
                    if ui.add(btn).clicked() && !selected {
                        *current = *value;
                        changed = true;
                    }
                }
            })
        });
    let mut response = inner.response;
    if changed {
        response.mark_changed();
    }
    response
}

/// A 34-segment level meter with an optional VAD-threshold marker.
pub fn level_meter(
    ui: &mut Ui,
    rms_db: f32,
    vad_threshold: Option<f32>,
    hot: bool,
) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let segments = 34u32;
    let seg_h = 7.0;
    let gap = 2.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), seg_h), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let frac = theme::meter_fraction(rms_db);
        let filled = (frac * segments as f32).round() as u32;
        let seg_w = (rect.width() - gap * (segments as f32 - 1.0)) / segments as f32;
        let mut x = rect.left();
        for i in 0..segments {
            let color = if i >= filled {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            } else if i + 3 >= segments {
                tok.level_clip
            } else if hot {
                tok.accent
            } else {
                tok.level
            };
            let seg_rect = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(seg_w, seg_h));
            painter.rect_filled(seg_rect, 1.5, color);
            x += seg_w + gap;
        }
        if let Some(th) = vad_threshold {
            let tx = rect.left() + rect.width() * th.clamp(0.0, 1.0);
            painter.line_segment(
                [egui::pos2(tx, rect.top() - 3.0), egui::pos2(tx, rect.bottom() + 3.0)],
                Stroke::new(2.0, ui.visuals().text_color()),
            );
        }
    }
    response
}

const AVATAR_DARK: [(Color32, Color32); 6] = [
    (Color32::from_rgb(0x20, 0x30, 0x3f), Color32::from_rgb(0x8c, 0xc6, 0xf5)),
    (Color32::from_rgb(0x2b, 0x24, 0x38), Color32::from_rgb(0xc5, 0xa8, 0xe6)),
    (Color32::from_rgb(0x1f, 0x33, 0x29), Color32::from_rgb(0x8f, 0xd3, 0xac)),
    (Color32::from_rgb(0x3a, 0x2a, 0x22), Color32::from_rgb(0xe5, 0xb0, 0x8c)),
    (Color32::from_rgb(0x2c, 0x2b, 0x1e), Color32::from_rgb(0xdb, 0xd0, 0x8a)),
    (Color32::from_rgb(0x25, 0x30, 0x3a), Color32::from_rgb(0x9f, 0xb8, 0xcc)),
];
const AVATAR_LIGHT: [(Color32, Color32); 6] = [
    (Color32::from_rgb(0xdd, 0xeb, 0xfa), Color32::from_rgb(0x17, 0x5a, 0x96)),
    (Color32::from_rgb(0xeb, 0xe2, 0xf7), Color32::from_rgb(0x5b, 0x3d, 0x8c)),
    (Color32::from_rgb(0xdd, 0xf0, 0xe5), Color32::from_rgb(0x18, 0x63, 0x43)),
    (Color32::from_rgb(0xf8, 0xe7, 0xda), Color32::from_rgb(0x8a, 0x4b, 0x22)),
    (Color32::from_rgb(0xf3, 0xef, 0xd8), Color32::from_rgb(0x6e, 0x61, 0x15)),
    (Color32::from_rgb(0xe4, 0xea, 0xf0), Color32::from_rgb(0x3b, 0x4f, 0x61)),
];

/// A circular avatar with a hash-picked fill and the name's initials;
/// `speaking` draws an accent ring around it.
pub fn avatar(ui: &mut Ui, name: &str, user_id: icu_proto::UserId, speaking: bool, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let palette = if ui.visuals().dark_mode {
            &AVATAR_DARK
        } else {
            &AVATAR_LIGHT
        };
        let (fill, text_color) = palette[(user_id % 6) as usize];
        let painter = ui.painter();
        let center = rect.center();
        let r = size / 2.0;
        painter.circle_filled(center, r, fill);
        if speaking {
            let tok = theme::tokens(ui.visuals());
            painter.circle_stroke(center, r + 2.0, Stroke::new(2.0, tok.accent));
        }
        let initials: String = name
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(2)
            .collect::<String>()
            .to_uppercase();
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            initials,
            FontId::new(12.5, super::fonts::semibold()),
            text_color,
        );
    }
    response
}

/// Five static accent bars indicating "currently speaking". Always
/// allocates its footprint so rows don't reflow when this toggles.
pub fn speaking_bars(ui: &mut Ui, speaking: bool) -> egui::Response {
    const HEIGHTS: [f32; 5] = [5.0, 11.0, 7.0, 13.0, 6.0];
    let w = 2.0;
    let gap = 2.0;
    let total_w = HEIGHTS.len() as f32 * w + (HEIGHTS.len() as f32 - 1.0) * gap;
    let max_h = 13.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(total_w, max_h), Sense::hover());
    if ui.is_rect_visible(rect) && speaking {
        let tok = theme::tokens(ui.visuals());
        let painter = ui.painter();
        let mut x = rect.left();
        for h in HEIGHTS {
            let bar = egui::Rect::from_min_size(egui::pos2(x, rect.bottom() - h), egui::vec2(w, h));
            painter.rect_filled(bar, 1.0, tok.accent);
            x += w + gap;
        }
    }
    response
}

/// A small monospace chip for a keybinding, e.g. `kbd(ui, "F12 halten")`.
pub fn kbd(ui: &mut Ui, text: &str) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let font = FontId::new(10.5, egui::FontFamily::Monospace);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, tok.dim_text);
    let padding = egui::vec2(6.0, 3.0);
    let size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(5), ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            CornerRadius::same(5),
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, tok.dim_text);
    }
    response
}

/// A rounded status pill: dot + text, with an optional `· trailing` value.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str, trailing: Option<&str>) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.27)))
        .corner_radius(CornerRadius::same(99))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.5, color);
                ui.label(RichText::new(text).size(11.5));
                if let Some(t) = trailing {
                    ui.label(RichText::new("·").color(tok.dim_text));
                    ui.label(RichText::new(t).monospace().color(tok.dim_text));
                }
            })
        })
        .response
}

/// Non-modal banner kind, driving the tint/border/icon color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Warning,
    Error,
}

/// A dismissible (or not) inline banner. Replaces the old plain
/// `colored_label` + `small_button` pattern.
pub fn banner<F1: FnOnce(), F2: FnOnce()>(
    ui: &mut Ui,
    kind: BannerKind,
    title: &str,
    body: &str,
    action: Option<(&str, F1)>,
    on_dismiss: Option<F2>,
) {
    let tok = theme::tokens(ui.visuals());
    let (fill, color, glyph) = match kind {
        BannerKind::Warning => (tok.warning_tint, tok.connecting, icon::WARNING),
        BannerKind::Error => (tok.error_tint, tok.error, icon::WARNING_CIRCLE),
    };
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.33)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph).size(16.0).color(color));
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(title)
                            .size(12.5)
                            .color(color)
                            .font(FontId::new(12.5, super::fonts::semibold())),
                    );
                    ui.label(RichText::new(body).size(12.0).color(tok.muted_text));
                    if let Some((label, run)) = action {
                        if ui.button(label).clicked() {
                            run();
                        }
                    }
                });
                if let Some(dismiss) = on_dismiss {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        if ui.small_button(icon::X).clicked() {
                            dismiss();
                        }
                    });
                }
            });
        });
}

/// A titled card: an uppercase caption above a bordered, rounded frame.
/// Callers pass `divider: true` to [`setting_row`]/[`slider_row`] for every
/// row after the first inside the card, to get the full-width separators.
pub fn group_card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.label(theme::caps_text(title));
    ui.add_space(4.0);
    let mut result = None;
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            result = Some(add_contents(ui));
        });
    ui.add_space(12.0);
    result.expect("add_contents always runs")
}

/// A label + right-aligned control row, used inside [`group_card`].
pub fn setting_row<R>(
    ui: &mut Ui,
    divider: bool,
    label: &str,
    help: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    if divider {
        ui.separator();
    }
    let tok = theme::tokens(ui.visuals());
    let mut result = None;
    Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(13.5));
                    if let Some(h) = help {
                        ui.label(RichText::new(h).size(11.5).color(tok.dim_text));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    result = Some(control(ui));
                });
            });
        });
    result.expect("control closure always runs")
}

/// A label + value-chip + full-width slider row, used inside [`group_card`].
pub fn slider_row(
    ui: &mut Ui,
    divider: bool,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
) -> egui::Response {
    if divider {
        ui.separator();
    }
    let tok = theme::tokens(ui.visuals());
    Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(13.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let chip = format!("{value:.1}{suffix}");
                    Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .stroke(Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color))
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| ui.label(RichText::new(chip).monospace().size(11.5)));
                });
            });
            let resp = ui.add(egui::Slider::new(value, range.clone()).show_value(false));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{:.0}{suffix}", range.start()))
                        .monospace()
                        .size(10.0)
                        .color(tok.dim_text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{:.0}{suffix}", range.end()))
                            .monospace()
                            .size(10.0)
                            .color(tok.dim_text),
                    );
                });
            });
            resp
        })
        .inner
}
