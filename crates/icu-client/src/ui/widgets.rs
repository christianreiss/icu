//! Small, theme-driven widget vocabulary shared by `main_window.rs` and
//! `settings.rs`. Everything the app draws comes from here, so the two
//! windows cannot drift apart: colors come from `theme::tokens(ui.visuals())`
//! plus the current `Visuals` — never a raw hex literal — and radii come
//! from `theme`'s four-step scale, never an invented number.
//!
//! The controls that carry state ([`toggle`], [`segmented`], [`slider`]) are
//! painted here rather than taken from stock egui, because stock egui draws
//! a slider's rail and its handle from the *same* `Visuals` field and the
//! two can therefore never be told apart.

use std::ops::RangeInclusive;

use egui::{Color32, CornerRadius, FontId, Frame, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_phosphor::regular as icon;

use super::theme;
use theme::{R_CARD, R_CHIP, R_CTRL, R_ROW};

/// Forces left-to-right child order for a widget group: plain `ui.horizontal`
/// inherits right-to-left from a right-aligned parent and silently reverses
/// its children. For a group inside a [`setting_row`] control slot use
/// [`row_group`] instead — this one would also drift to the left edge of the
/// slot, because a left-to-right child `Ui` starts at its parent's left.
pub fn ltr<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    ui.with_layout(
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    )
}

/// A group of widgets inside a [`setting_row`]'s right-aligned control slot:
/// laid out left-to-right in reading order, but as a block of exactly
/// `width`, which is what keeps it flush with the toggles and combo boxes in
/// the rows above and below.
///
/// egui has no way to measure a closure before running it, so `width` has to
/// be told, not discovered — the `*_width` helpers in this module exist so a
/// call site can add up its own content instead of guessing. A `width` that
/// is too large simply leaves the group hanging left of the edge.
pub fn row_group<R>(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.available_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    )
    .inner
}

/// Width of `text` in `font`, for the widgets that have to know their own
/// size before they are placed.
fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), font, Color32::WHITE)
        .size()
        .x
}

/// The width [`button`] will take for this glyph and label.
pub fn button_width(ui: &Ui, glyph: &str, label: &str) -> f32 {
    text_width(
        ui,
        &caption(glyph, label),
        egui::TextStyle::Button.resolve(ui.style()),
    ) + ui.spacing().button_padding.x * 2.0
}

/// `glyph  label`, with either half allowed to be empty.
fn caption(glyph: &str, label: &str) -> String {
    match (glyph.is_empty(), label.is_empty()) {
        (true, _) => label.to_string(),
        (false, true) => glyph.to_string(),
        (false, false) => format!("{glyph}  {label}"),
    }
}

/// Right-aligns a group inside the remaining width of a row, at an explicit
/// height. The explicit height matters: a plain `with_layout` claims the
/// parent's *entire* remaining height and `Align::Center` then centers the
/// content inside that tall rect, which balloons the row and pushes
/// everything after it off-screen.
pub fn rtl<R>(ui: &mut Ui, height: f32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::right_to_left(egui::Align::Center),
        add_contents,
    )
    .inner
}

/// A note dropped directly inside a [`group_card`] — error text, help
/// blurbs, anything that isn't a label+control [`setting_row`] — inset by
/// the same 12px the rows use, so it doesn't sit flush against the card
/// border.
pub fn card_note<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, add_contents)
        .inner
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        egui::lerp(a.r() as f32..=b.r() as f32, t).round() as u8,
        egui::lerp(a.g() as f32..=b.g() as f32, t).round() as u8,
        egui::lerp(a.b() as f32..=b.b() as f32, t).round() as u8,
        egui::lerp(a.a() as f32..=b.a() as f32, t).round() as u8,
    )
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// What a [`button`] is for, which is all a call site has to decide: the
/// fill, border and label color follow from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one action a panel exists for (apply a name, mint an invite).
    Primary,
    /// Everything else.
    Neutral,
    /// Destructive and irreversible (revoke a user).
    Danger,
}

/// A button with an optional leading icon. The whole app goes through this
/// instead of `ui.button`, so a button never picks up stock egui's chrome.
pub fn button(ui: &mut Ui, kind: ButtonKind, glyph: &str, label: &str) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (fill, border, text) = match kind {
        ButtonKind::Primary => (tok.accent, tok.accent, tok.on_accent),
        ButtonKind::Neutral => (
            ui.visuals().widgets.inactive.weak_bg_fill,
            ui.visuals().widgets.inactive.bg_stroke.color,
            ui.visuals().text_color(),
        ),
        ButtonKind::Danger => (tok.error_tint, tok.error.gamma_multiply(0.45), tok.error),
    };
    let response = ui.add(
        egui::Button::new(RichText::new(caption(glyph, label)).color(text))
            .fill(fill)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(R_CTRL))
            .min_size(egui::vec2(0.0, 30.0)),
    );
    // `Button::fill` is used for every interaction state, so hover has to be
    // drawn here or the button would be visually dead under the cursor.
    if response.hovered() {
        ui.painter().rect_filled(
            response.rect,
            CornerRadius::same(R_CTRL),
            Color32::from_white_alpha(12),
        );
    }
    response
}

/// A square, quiet icon button — the header's gear, a row's volume handle.
/// `active` tints it so a toggled-open state is visible without a label.
pub fn icon_button(ui: &mut Ui, glyph: &str, size: f32, active: bool) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, color) = if active {
            (tok.accent_tint, tok.accent)
        } else if response.hovered() {
            (ui.visuals().faint_bg_color, ui.visuals().text_color())
        } else {
            (Color32::TRANSPARENT, tok.muted_text)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(R_CTRL), fill);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::new((size * 0.5).round(), egui::FontFamily::Proportional),
            color,
        );
    }
    response
}

// ---------------------------------------------------------------------------
// Stateful controls
// ---------------------------------------------------------------------------

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
        painter.circle_filled(
            egui::pos2(cx, rect.center().y),
            knob_r,
            lerp_color(tok.dim_text, knob_color(ui.visuals()), t),
        );
    }
    response
}

/// The light face of a moving part (toggle knob, slider knob).
fn knob_color(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(0xf2, 0xf6, 0xfb)
    } else {
        Color32::WHITE
    }
}

/// Inside a [`segmented`] track: padding on each side of an option's label,
/// the gap between options, and the track's own inset.
const SEG_PAD: f32 = 11.0;
const SEG_GAP: f32 = 2.0;
const SEG_INSET: f32 = 3.0;
const SEG_H: f32 = 22.0;

/// The label font inside a [`segmented`] track.
fn seg_font() -> FontId {
    FontId::new(12.5, super::fonts::semibold())
}

/// The width [`segmented`] will take for these options.
pub fn segmented_width(ui: &Ui, labels: &[&str]) -> f32 {
    let options: f32 = labels
        .iter()
        .map(|l| text_width(ui, l, seg_font()) + SEG_PAD * 2.0)
        .sum();
    options + SEG_GAP * (labels.len() as f32 - 1.0) + SEG_INSET * 2.0
}

/// A segmented control: a track of mutually exclusive options. Returns a
/// `Response` whose `.changed()` reports a switch; the new value is written
/// into `current` directly. Sizes itself to its options, so the track never
/// stretches across an empty row.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    options: &[(T, &str)],
) -> egui::Response {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let width = segmented_width(ui, &labels);
    let mut changed = false;
    let inner = row_group(ui, width, |ui| {
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.inactive.bg_stroke.color,
            ))
            .corner_radius(CornerRadius::same(R_CTRL))
            .inner_margin(SEG_INSET as i8)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = SEG_GAP;
                // Must match what `segmented_width` assumed, or the track
                // overhangs its column and stands taller than the toggles
                // in the rows above it.
                ui.spacing_mut().button_padding = egui::vec2(SEG_PAD, 3.0);
                for (value, label) in options {
                    let selected = *current == *value;
                    let seg = text_width(ui, label, seg_font()) + SEG_PAD * 2.0;
                    let btn = egui::Button::new(RichText::new(*label).font(seg_font()))
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
                        .corner_radius(CornerRadius::same(R_CHIP))
                        .min_size(egui::vec2(seg, SEG_H));
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

/// A slider drawn in the same hand as [`toggle`]: a thin recessed track, an
/// accent trail up to the value, a light knob. Fills the available width and
/// takes a click or a drag anywhere along the row.
pub fn slider(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>) -> egui::Response {
    const HEIGHT: f32 = 22.0;
    const KNOB: f32 = 8.0;
    const TRACK: f32 = 5.0;

    let width = ui.available_width().max(KNOB * 4.0);
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(width, HEIGHT), Sense::click_and_drag());
    // The knob centre never leaves the track, so the value stays reachable
    // at both ends without the knob hanging over the edge.
    let span = (rect.left() + KNOB)..=(rect.right() - KNOB);
    if let Some(pos) = response.interact_pointer_pos() {
        let t = egui::remap_clamp(pos.x, span.clone(), 0.0..=1.0);
        let picked = egui::lerp(range.clone(), t);
        if picked != *value {
            *value = picked;
            response.mark_changed();
        }
    }
    if ui.is_rect_visible(rect) {
        let tok = theme::tokens(ui.visuals());
        let t = egui::remap_clamp(*value, range, 0.0..=1.0);
        let cx = egui::lerp(span, t);
        let painter = ui.painter();
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), TRACK));
        painter.rect_filled(track, TRACK / 2.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            track,
            TRACK / 2.0,
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        let mut trail = track;
        trail.max.x = cx;
        painter.rect_filled(trail, TRACK / 2.0, tok.accent);
        let hot = response.hovered() || response.dragged();
        painter.circle(
            egui::pos2(cx, rect.center().y),
            if hot { KNOB + 1.0 } else { KNOB },
            knob_color(ui.visuals()),
            Stroke::new(1.0, tok.accent.gamma_multiply(if hot { 1.0 } else { 0.4 })),
        );
    }
    response
}

// ---------------------------------------------------------------------------
// Audio indicators
// ---------------------------------------------------------------------------

/// The microphone level, as 28 segments over −60…0 dBFS plus a one-segment
/// peak marker. `hot` means the gate is open and audio is actually leaving
/// this client, which recolors the bar from "level" to "accent" — the
/// difference between "the microphone hears you" and "they hear you".
pub fn level_meter(ui: &mut Ui, rms_db: f32, peak_db: f32, hot: bool) -> egui::Response {
    const SEGMENTS: u32 = 28;
    const HEIGHT: f32 = 9.0;
    const GAP: f32 = 2.0;

    let tok = theme::tokens(ui.visuals());
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), HEIGHT), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let seg_w = (rect.width() - GAP * (SEGMENTS as f32 - 1.0)) / SEGMENTS as f32;
        let upto = |db: f32| (theme::meter_fraction(db) * SEGMENTS as f32).round() as u32;
        let filled = upto(rms_db);
        // A peak below the meter's own floor has no segment to sit on, so
        // it lights nothing — otherwise digital silence would still show a
        // marker parked on the first segment.
        let peak = (peak_db > -60.0).then(|| upto(peak_db).clamp(1, SEGMENTS) - 1);
        let empty = ui.visuals().widgets.noninteractive.bg_stroke.color;
        for i in 0..SEGMENTS {
            let color = if i + 3 >= SEGMENTS {
                tok.level_clip
            } else if hot {
                tok.accent
            } else {
                tok.level
            };
            let color = if i < filled {
                color
            } else if peak == Some(i) {
                color.gamma_multiply(0.55)
            } else {
                empty
            };
            let seg = egui::Rect::from_min_size(
                egui::pos2(rect.left() + i as f32 * (seg_w + GAP), rect.top()),
                egui::vec2(seg_w, HEIGHT),
            );
            painter.rect_filled(seg, 2.0, color);
        }
    }
    response
}

/// Voice-activation readout: the current speech probability as a small bar,
/// with a tick at the threshold it has to cross. Probability and threshold
/// share the 0…1 axis — unlike the dBFS [`level_meter`], which is why this
/// is a separate widget rather than a marker on that one.
pub fn vad_bar(ui: &mut Ui, prob: f32, threshold: f32) -> egui::Response {
    const WIDTH: f32 = 72.0;
    const HEIGHT: f32 = 7.0;

    let tok = theme::tokens(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(WIDTH, HEIGHT + 4.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(WIDTH, HEIGHT));
        painter.rect_filled(track, HEIGHT / 2.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            track,
            HEIGHT / 2.0,
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        let mut fill = track;
        fill.max.x = track.left() + track.width() * prob.clamp(0.0, 1.0);
        painter.rect_filled(
            fill,
            HEIGHT / 2.0,
            if prob >= threshold {
                tok.accent
            } else {
                tok.dim_text
            },
        );
        let tx = track.left() + track.width() * threshold.clamp(0.0, 1.0);
        painter.line_segment(
            [egui::pos2(tx, rect.top()), egui::pos2(tx, rect.bottom())],
            Stroke::new(2.0, ui.visuals().text_color()),
        );
    }
    response
}

/// Five static accent bars indicating "currently speaking". Always
/// allocates its footprint so rows don't reflow when this toggles.
pub fn speaking_bars(ui: &mut Ui) -> egui::Response {
    const HEIGHTS: [f32; 5] = [5.0, 11.0, 7.0, 13.0, 6.0];
    let w = 2.0;
    let gap = 2.0;
    let total_w = HEIGHTS.len() as f32 * w + (HEIGHTS.len() as f32 - 1.0) * gap;
    let max_h = 13.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(total_w, max_h), Sense::hover());
    if ui.is_rect_visible(rect) {
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

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Up to two characters for an [`avatar`]: the initials of the first two
/// words, or — for a one-word name — its first letter plus a trailing digit
/// if there is one, so `chris1` and `chris2` don't both collapse to `CH`.
pub fn initials(name: &str) -> String {
    let mut words = name.split_whitespace();
    let Some(first) = words.next().and_then(|w| w.chars().next()) else {
        return "?".to_string();
    };
    let second = words
        .next()
        .and_then(|w| w.chars().next())
        .or_else(|| name.chars().last().filter(char::is_ascii_digit));
    match second {
        Some(c) => format!("{first}{c}").to_uppercase(),
        None => first.to_uppercase().to_string(),
    }
}

const AVATAR_DARK: [(Color32, Color32); 6] = [
    (
        Color32::from_rgb(0x20, 0x30, 0x3f),
        Color32::from_rgb(0x8c, 0xc6, 0xf5),
    ),
    (
        Color32::from_rgb(0x2b, 0x24, 0x38),
        Color32::from_rgb(0xc5, 0xa8, 0xe6),
    ),
    (
        Color32::from_rgb(0x1f, 0x33, 0x29),
        Color32::from_rgb(0x8f, 0xd3, 0xac),
    ),
    (
        Color32::from_rgb(0x3a, 0x2a, 0x22),
        Color32::from_rgb(0xe5, 0xb0, 0x8c),
    ),
    (
        Color32::from_rgb(0x2c, 0x2b, 0x1e),
        Color32::from_rgb(0xdb, 0xd0, 0x8a),
    ),
    (
        Color32::from_rgb(0x25, 0x30, 0x3a),
        Color32::from_rgb(0x9f, 0xb8, 0xcc),
    ),
];
const AVATAR_LIGHT: [(Color32, Color32); 6] = [
    (
        Color32::from_rgb(0xdd, 0xeb, 0xfa),
        Color32::from_rgb(0x17, 0x5a, 0x96),
    ),
    (
        Color32::from_rgb(0xeb, 0xe2, 0xf7),
        Color32::from_rgb(0x5b, 0x3d, 0x8c),
    ),
    (
        Color32::from_rgb(0xdd, 0xf0, 0xe5),
        Color32::from_rgb(0x18, 0x63, 0x43),
    ),
    (
        Color32::from_rgb(0xf8, 0xe7, 0xda),
        Color32::from_rgb(0x8a, 0x4b, 0x22),
    ),
    (
        Color32::from_rgb(0xf3, 0xef, 0xd8),
        Color32::from_rgb(0x6e, 0x61, 0x15),
    ),
    (
        Color32::from_rgb(0xe4, 0xea, 0xf0),
        Color32::from_rgb(0x3b, 0x4f, 0x61),
    ),
];

/// A circular avatar with a hash-picked fill and the name's [`initials`];
/// `speaking` draws an accent ring around it.
pub fn avatar(
    ui: &mut Ui,
    name: &str,
    user_id: icu_proto::UserId,
    speaking: bool,
    size: f32,
) -> egui::Response {
    // The ring sits outside the circle; reserving room for it keeps the row
    // from shifting when somebody starts talking.
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size + 6.0, size + 6.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let palette = if ui.visuals().dark_mode {
            &AVATAR_DARK
        } else {
            &AVATAR_LIGHT
        };
        let (fill, text_color) = palette[(user_id % 6) as usize];
        let painter = ui.painter();
        let center = rect.center();
        painter.circle_filled(center, size / 2.0, fill);
        if speaking {
            let tok = theme::tokens(ui.visuals());
            painter.circle_stroke(center, size / 2.0 + 2.0, Stroke::new(2.0, tok.accent));
        }
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            initials(name),
            FontId::new(size * 0.37, super::fonts::semibold()),
            text_color,
        );
    }
    response
}

// ---------------------------------------------------------------------------
// Chips, pills, banners
// ---------------------------------------------------------------------------

/// A small monospace chip for a value or a keybinding, e.g.
/// `chip(ui, "F12 halten", 0.0)`. `min_width` reserves a fixed box so a
/// changing number doesn't shove its neighbours around every frame.
pub fn chip(ui: &mut Ui, text: &str, min_width: f32) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let font = FontId::new(11.0, egui::FontFamily::Monospace);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, tok.dim_text);
    let padding = egui::vec2(7.0, 3.0);
    let size = egui::vec2(
        (galley.size().x + padding.x * 2.0).max(min_width),
        galley.size().y + padding.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius::same(R_CHIP),
            ui.visuals().extreme_bg_color,
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(R_CHIP),
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, tok.dim_text);
    }
    response
}

/// The width [`chip`] will take for the widest of `samples`, so a row can
/// reserve it up front.
pub fn chip_width(ui: &Ui, samples: &[String]) -> f32 {
    let font = FontId::new(11.0, egui::FontFamily::Monospace);
    samples
        .iter()
        .map(|s| {
            ui.painter()
                .layout_no_wrap(s.clone(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0f32, f32::max)
        + 14.0
}

/// A rounded status pill: a colored dot plus one short word. Anything longer
/// than a word belongs in the caller's `on_hover_text`, not in here.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str) -> egui::Response {
    const PAD: f32 = 9.0;
    const DOT: f32 = 7.0;
    const GAP: f32 = 5.0;

    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, ui.visuals().text_color());
    let size = egui::vec2(
        PAD * 2.0 + DOT + GAP + galley.size().x,
        galley.size().y.max(DOT) + 7.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, rect.height() / 2.0, color.gamma_multiply(0.16));
        painter.rect_stroke(
            rect,
            rect.height() / 2.0,
            Stroke::new(1.0, color.gamma_multiply(0.27)),
            StrokeKind::Inside,
        );
        painter.circle_filled(
            egui::pos2(rect.left() + PAD + DOT / 2.0, rect.center().y),
            DOT / 2.0,
            color,
        );
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(
            egui::pos2(rect.left() + PAD + DOT + GAP, y),
            galley,
            ui.visuals().text_color(),
        );
    }
    response
}

/// Non-modal banner kind, driving the tint/border/icon color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// Something needs attention but the app still works.
    Warning,
    /// Something failed.
    Error,
}

/// A dismissible (or not) inline banner.
pub fn banner<F: FnOnce()>(
    ui: &mut Ui,
    kind: BannerKind,
    title: &str,
    body: &str,
    on_dismiss: Option<F>,
) {
    let tok = theme::tokens(ui.visuals());
    let (fill, color, glyph) = match kind {
        BannerKind::Warning => (tok.warning_tint, tok.connecting, icon::WARNING),
        BannerKind::Error => (tok.error_tint, tok.error, icon::WARNING_CIRCLE),
    };
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.33)))
        .corner_radius(CornerRadius::same(R_ROW))
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
                });
                if let Some(dismiss) = on_dismiss {
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 22.0),
                        egui::Layout::right_to_left(egui::Align::TOP),
                        |ui| {
                            if icon_button(ui, icon::X, 22.0, false).clicked() {
                                dismiss();
                            }
                        },
                    );
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Settings layout
// ---------------------------------------------------------------------------

/// A titled card: an uppercase caption above a bordered, rounded frame.
/// Callers pass `divider: true` to [`setting_row`]/[`slider_row`] for every
/// row after the first inside the card, to get the full-width separators.
pub fn group_card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.label(theme::caps_text(title));
    ui.add_space(4.0);
    let mut result = None;
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.inactive.bg_stroke.color,
        ))
        .corner_radius(CornerRadius::same(R_CARD))
        .show(ui, |ui| {
            result = Some(add_contents(ui));
        });
    ui.add_space(14.0);
    result.expect("add_contents always runs")
}

/// How wide a [`setting_row`] label column is, so labels and the controls
/// beside them line up down a whole card.
const LABEL_W: f32 = 150.0;

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
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(13.5));
                    if let Some(h) = help {
                        ui.label(RichText::new(h).size(11.5).color(tok.dim_text));
                    }
                });
                result = Some(rtl(ui, 30.0, control));
            });
        });
    result.expect("control closure always runs")
}

/// A label + slider + value-chip row on a single line, used inside
/// [`group_card`]. The chip is pre-sized from the range's widest end so the
/// slider doesn't change width as the value is dragged.
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
    let format = |v: f32| {
        if suffix == " ms" || suffix == " %" {
            format!("{v:.0}{suffix}")
        } else {
            format!("{v:.1}{suffix}")
        }
    };
    let reserve = chip_width(ui, &[format(*range.start()), format(*range.end())]);
    Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // An exact allocation, not `allocate_ui_with_layout`: that
                // one hands back only the space its content used, so every
                // slider would start at a different x.
                let (col, _) = ui.allocate_exact_size(egui::vec2(LABEL_W, 22.0), Sense::hover());
                ui.painter().text(
                    col.left_center(),
                    egui::Align2::LEFT_CENTER,
                    label,
                    FontId::new(13.5, egui::FontFamily::Proportional),
                    ui.visuals().text_color(),
                );
                rtl(ui, 22.0, |ui| {
                    chip(ui, &format(*value), reserve);
                    slider(ui, value, range)
                })
            })
            .inner
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_distinguish_numbered_names() {
        assert_eq!(initials("chris1"), "C1");
        assert_eq!(initials("chris2"), "C2");
        assert_eq!(initials("Christian Reiß"), "CR");
        assert_eq!(initials("mara"), "M");
        assert_eq!(initials(""), "?");
        assert_eq!(initials("   "), "?");
    }
}
