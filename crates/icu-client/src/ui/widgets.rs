//! Small, theme-driven widget vocabulary shared by `main_window.rs` and
//! `settings.rs`. Everything the app draws comes from here, so the two
//! windows cannot drift apart: colors come from `theme::tokens(ui.visuals())`
//! plus the current `Visuals` — never a raw hex literal — and radii come
//! from `theme`'s four-step scale, never an invented number.
//!
//! The controls that carry state ([`named_toggle`], [`segmented`], [`named_slider`]) are
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
        .show(ui, |ui| {
            // `group_card` zeroes item spacing for its rows; a note's
            // paragraphs still need a little air between them.
            ui.spacing_mut().item_spacing.y = 4.0;
            add_contents(ui)
        })
        .inner
}

/// One quiet line of help under a card's rows, as a [`card_note`]: the
/// shape every "what this means" blurb in the settings takes.
pub fn help_note(ui: &mut Ui, text: &str) {
    card_note(ui, |ui| {
        ui.label(
            RichText::new(text)
                .size(11.5)
                .color(theme::tokens(ui.visuals()).dim_text),
        );
    });
}

/// The recessed surface a value sits in — a chip, a preview, a picker
/// swatch: the sunken fill with a hairline border.
pub fn paint_well(ui: &Ui, rect: egui::Rect, radius: u8) {
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(radius),
        ui.visuals().extreme_bg_color,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
        StrokeKind::Inside,
    );
}

/// A fixed-width, left-aligned label column. An exact allocation, not
/// `allocate_ui_with_layout`: that one hands back only the space its content
/// used, so every control after it would start at a different x. It also
/// sets the row height before the controls arrive, so they center on it.
pub fn label_column(ui: &mut Ui, width: f32, height: f32, label: &str) {
    let (col, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    ui.painter().text(
        col.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::new(13.5, egui::FontFamily::Proportional),
        ui.visuals().text_color(),
    );
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
        ui.painter()
            .rect_filled(response.rect, CornerRadius::same(R_CTRL), tok.hover_overlay);
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
            (tok.hover_overlay, ui.visuals().text_color())
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

/// Which color a [`tile`] takes when it is on: `Danger` for the switches
/// that take something away (mute, deafen), `Accent` for the ones that add
/// something (camera, overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Accent,
    Danger,
}

/// The dot on a [`tile`]: whether this client's own video is going out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecDot {
    /// No dot: the tile has nothing to do with video, or the camera is off.
    Off,
    /// A quiet grey dot: the camera is on, but nothing leaves this client —
    /// nobody is watching, no connection, or the network paused video.
    Idle,
    /// A pulsing red dot: somebody is seeing this client's picture right now.
    Live,
}

/// One cell of the main window's control dock: an icon over a short label.
/// Off is a quiet raised surface; on is a tint in its [`Role`] color, with
/// icon and label in that color — never a solid fill, so a row of them stays
/// calm. `rec` puts a small dot at the top right; see [`RecDot`].
pub fn tile(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    active: bool,
    role: Role,
    rec: RecDot,
    size: egui::Vec2,
) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), active, label)
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, active);
        let (on, on_tint) = match role {
            Role::Accent => (tok.accent, tok.accent_tint_2),
            Role::Danger => (tok.error, tok.error_tint),
        };
        let painter = ui.painter();
        painter.rect_filled(rect, R_ROW, lerp_color(tok.surface_hi, on_tint, t));
        if t > 0.0 {
            painter.rect_stroke(
                rect,
                R_ROW,
                Stroke::new(1.0, on.gamma_multiply(0.45 * t)),
                StrokeKind::Inside,
            );
        }
        if response.hovered() {
            painter.rect_filled(rect, R_ROW, tok.hover_overlay);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let idle = if response.hovered() {
            ui.visuals().text_color()
        } else {
            tok.muted_text
        };
        let color = lerp_color(idle, on, t);
        painter.text(
            egui::pos2(rect.center().x, rect.center().y - 8.0),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::new(19.0, egui::FontFamily::Proportional),
            color,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.center().y + 12.0),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(10.5, super::fonts::semibold()),
            color,
        );
        let pos = egui::pos2(rect.right() - 10.0, rect.top() + 10.0);
        match rec {
            RecDot::Off => {}
            RecDot::Idle => {
                painter.circle_filled(pos, 3.0, tok.muted_text);
            }
            RecDot::Live => {
                let time = ui.input(|i| i.time);
                let pulse = ((time * 2.4).sin() as f32 * 0.5 + 0.5).clamp(0.0, 1.0);
                painter.circle_filled(pos, 3.0 + pulse, tok.error);
            }
        }
    }
    response
}

/// What the speak surface shows; see [`speak_surface`].
pub struct Speak<'a> {
    /// Push-to-talk (`true`) or voice activation.
    pub ptt: bool,
    /// The gate is open and audio is leaving this client.
    pub open: bool,
    pub muted: bool,
    /// No connection: nothing can leave this client, whatever the gate or
    /// the key says, so the surface never lights up.
    pub offline: bool,
    /// Connected, but no one else is in the room: nothing is sent, so the
    /// surface stays dark like offline and says why.
    pub alone: bool,
    /// 0…1: the microphone level in PTT mode, the speech probability in
    /// voice activation — whatever decides whether the gate opens.
    pub level: f32,
    /// The voice-activation threshold on the same axis as `level`.
    pub threshold: Option<f32>,
    /// The push-to-talk key, shown as a keycap beside the label.
    pub key: Option<&'a str>,
    /// Muted, yet talking (voice heard, or the PTT key held): the surface
    /// says so instead of letting people talk into the void (Mumble's
    /// "mute cue").
    pub talking_muted: bool,
    /// The microphone clips: the level line turns red.
    pub clipping: bool,
}

/// The big control: the speak button in push-to-talk, a readout in voice
/// activation. A short label in the middle and a thin level line along the
/// bottom edge; open, the whole surface turns accent. Senses clicks only in
/// PTT mode, so a caller's hold logic can keep reading
/// `is_pointer_button_down_on` off the returned response.
pub fn speak_surface(ui: &mut Ui, s: &Speak<'_>, size: egui::Vec2) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let sense = if s.ptt {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Sprechen")
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let open = ui
        .ctx()
        .animate_bool_with_time(response.id, s.open && !s.offline && !s.alone, 0.08);
    let hot = open > 0.5;
    painter.rect_filled(rect, R_ROW, lerp_color(tok.surface_hi, tok.accent, open));
    if s.talking_muted {
        let time = ui.input(|i| i.time);
        let pulse = ((time * 3.0).sin() as f32 * 0.5 + 0.5).clamp(0.0, 1.0);
        // No repaint request of its own: the main window repaints every
        // 33 ms anyway, which is plenty for this pulse.
        painter.rect_filled(
            rect,
            R_ROW,
            tok.error_tint.gamma_multiply(0.6 + 0.4 * pulse),
        );
    }
    if s.ptt && !s.offline && !s.alone && response.hovered() {
        painter.rect_filled(rect, R_ROW, tok.hover_overlay);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // The level line, eased so it glides instead of flickering at 30 fps.
    // Muted, the line still moves (dimmed): it shows the microphone works,
    // just that nothing goes out.
    let level =
        ui.ctx()
            .animate_value_with_time(response.id.with("level"), s.level.clamp(0.0, 1.0), 0.12);
    let line = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 14.0, rect.bottom() - 9.0),
        egui::pos2(rect.right() - 14.0, rect.bottom() - 6.0),
    );
    let (track, fill) = if hot {
        (
            tok.on_accent.gamma_multiply(0.18),
            tok.on_accent.gamma_multiply(0.7),
        )
    } else if s.muted {
        (
            ui.visuals().widgets.inactive.bg_stroke.color,
            tok.error.gamma_multiply(0.5),
        )
    } else {
        (
            ui.visuals().widgets.inactive.bg_stroke.color,
            if s.ptt { tok.level } else { tok.accent },
        )
    };
    let fill = if s.clipping && !s.muted {
        tok.error
    } else {
        fill
    };
    painter.rect_filled(line, 1.5, track);
    if level > 0.0 {
        let mut bar = line;
        bar.max.x = line.left() + line.width() * level;
        painter.rect_filled(bar, 1.5, fill);
    }
    if let Some(th) = s.threshold {
        let x = line.left() + line.width() * th.clamp(0.0, 1.0);
        painter.line_segment(
            [
                egui::pos2(x, line.top() - 2.0),
                egui::pos2(x, line.bottom() + 2.0),
            ],
            Stroke::new(1.5, if hot { tok.on_accent } else { tok.muted_text }),
        );
    }

    let (glyph, label, color) = if s.talking_muted {
        (icon::MICROPHONE_SLASH, "Stumm — du sprichst", tok.error)
    } else if s.muted {
        (icon::MICROPHONE_SLASH, "Stumm", tok.error)
    } else if s.offline {
        (icon::PLUGS, "Getrennt", tok.muted_text)
    } else if s.alone {
        (icon::USERS, "Allein", tok.muted_text)
    } else if hot {
        (icon::MICROPHONE, "Sendet", tok.on_accent)
    } else if s.ptt {
        (icon::MICROPHONE, "Halten", ui.visuals().text_color())
    } else {
        (icon::MICROPHONE, "Bereit", tok.muted_text)
    };
    let text = painter.layout_no_wrap(
        format!("{glyph}  {label}"),
        FontId::new(14.0, super::fonts::semibold()),
        color,
    );
    let key = s
        .key
        .filter(|_| !hot && !s.muted && !s.offline && !s.alone)
        .map(|k| {
            painter.layout_no_wrap(
                k.to_string(),
                FontId::new(10.5, egui::FontFamily::Monospace),
                tok.dim_text,
            )
        });
    const KEY_PAD: egui::Vec2 = egui::vec2(6.0, 2.0);
    const KEY_GAP: f32 = 8.0;
    let key_w = key
        .as_ref()
        .map_or(0.0, |k| k.size().x + KEY_PAD.x * 2.0 + KEY_GAP);
    let cy = rect.center().y - 3.0;
    let left = rect.center().x - (text.size().x + key_w) / 2.0;
    let text_w = text.size().x;
    painter.galley(egui::pos2(left, cy - text.size().y / 2.0), text, color);
    if let Some(k) = key {
        let cap = egui::Rect::from_min_size(
            egui::pos2(left + text_w + KEY_GAP, cy - k.size().y / 2.0 - KEY_PAD.y),
            k.size() + KEY_PAD * 2.0,
        );
        painter.rect_stroke(
            cap,
            CornerRadius::same(R_CHIP),
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Inside,
        );
        painter.galley(cap.min + KEY_PAD, k, tok.dim_text);
    }
    response
}

// ---------------------------------------------------------------------------
// Main window header
// ---------------------------------------------------------------------------

/// Height of the [`status_bar`].
pub const STATUS_BAR_H: f32 = 20.0;
/// Height of an [`action_capsule`], and with it the header's title row.
pub const CAPSULE_H: f32 = 32.0;
/// Inset of the header's content from its edges: the status bar's text and
/// the title start where the rows in the list card below do.
pub const HEADER_INSET: f32 = 8.0;
/// Room between two things in the [`status_bar`].
const STATUS_GAP: f32 = 6.0;
/// Width of the box [`signal_bars`] draws into.
pub const SIGNAL_BARS_W: f32 = 22.0;

/// Five signal bars, the lowest `lit` of them colored by how many that is
/// (4–5 good, 2–3 amber, 1 bad), the rest faint — like a phone's reception.
/// Painted into `rect`, [`SIGNAL_BARS_W`] wide and bottom-aligned 6 px below
/// its center.
pub fn signal_bars(ui: &Ui, rect: egui::Rect, lit: u8) {
    let tok = theme::tokens(ui.visuals());
    let on = match lit {
        4.. => tok.connected,
        2..=3 => tok.connecting,
        _ => tok.error,
    };
    let off = tok.dim_text.gamma_multiply(0.25);
    for i in 0..5u8 {
        let h = 4.0 + 2.5 * f32::from(i);
        let x = rect.left() + 2.0 + 4.0 * f32::from(i);
        let bar = egui::Rect::from_min_max(
            egui::pos2(x, rect.center().y + 6.0 - h),
            egui::pos2(x + 2.5, rect.center().y + 6.0),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(1), if i < lit { on } else { off });
    }
}

/// What the [`status_bar`] shows.
pub struct StatusBar<'a> {
    /// The dot's color: the connection's state.
    pub color: Color32,
    /// The state in one word.
    pub word: &'a str,
    /// Quiet text after the word: how long the connection has been up.
    pub extra: Option<&'a str>,
    /// Signal bars (1–5) and the round trip they stand for, in ms.
    pub signal: Option<(u8, u16)>,
    /// Glyphs left of the signal, each in its own color.
    pub indicators: &'a [(&'a str, Color32)],
}

/// The top line of the main window, the way a phone's status bar reads:
/// the connection's state on the left, reception and indicators on the
/// right, all in one small size on one [`STATUS_BAR_H`] line. Clickable as a
/// whole; anything longer belongs in the caller's tooltip.
pub fn status_bar(ui: &mut Ui, s: &StatusBar<'_>) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let size = egui::vec2(ui.available_width(), STATUS_BAR_H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), s.word));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, R_CHIP, tok.hover_overlay);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let text = |text: &str, font: FontId, color: Color32| {
        painter.layout_no_wrap(text.to_string(), font, color)
    };
    let regular = FontId::new(11.5, egui::FontFamily::Proportional);
    let mid = rect.center().y;
    let at = |x: f32, g: &egui::Galley| egui::pos2(x, mid - g.size().y / 2.0);

    let mut x = rect.left() + HEADER_INSET;
    painter.circle_filled(egui::pos2(x + 3.5, mid), 3.5, s.color);
    x += 7.0 + STATUS_GAP;
    let word = text(
        s.word,
        FontId::new(11.5, super::fonts::semibold()),
        ui.visuals().text_color(),
    );
    let word_w = word.size().x;
    painter.galley(at(x, &word), word, ui.visuals().text_color());
    x += word_w;
    if let Some(extra) = s.extra {
        let extra = text(&format!("  ·  {extra}"), regular.clone(), tok.muted_text);
        painter.galley(at(x, &extra), extra, tok.muted_text);
    }

    let mut x = rect.right() - HEADER_INSET;
    if let Some((lit, ms)) = s.signal {
        let ms = text(&format!("{ms} ms"), regular, tok.muted_text);
        x -= ms.size().x;
        painter.galley(at(x, &ms), ms, tok.muted_text);
        x -= 2.0 + SIGNAL_BARS_W;
        let bars = egui::Rect::from_min_size(
            egui::pos2(x, rect.top()),
            egui::vec2(SIGNAL_BARS_W, STATUS_BAR_H),
        );
        signal_bars(ui, bars, lit);
        x -= STATUS_GAP;
    }
    for &(glyph, color) in s.indicators {
        let g = text(
            glyph,
            FontId::new(13.0, egui::FontFamily::Proportional),
            color,
        );
        x -= g.size().x;
        painter.galley(at(x, &g), g, color);
        x -= STATUS_GAP;
    }
    response
}

/// One button of an [`action_capsule`].
pub struct CapsuleButton<'a> {
    pub glyph: &'a str,
    /// Its tooltip and accessible name.
    pub label: &'a str,
    /// Tinted in the accent: what it opens is open.
    pub active: bool,
    /// Turns the error color under the pointer: it ends something.
    pub danger: bool,
}

/// Room around the buttons inside an [`action_capsule`].
const CAPSULE_PAD: f32 = 2.0;
/// One round button of an [`action_capsule`].
const CAPSULE_BUTTON: f32 = CAPSULE_H - 2.0 * CAPSULE_PAD;

/// How wide an [`action_capsule`] with `n` buttons is.
pub fn action_capsule_width(n: usize) -> f32 {
    n as f32 * CAPSULE_BUTTON + 2.0 * CAPSULE_PAD
}

/// A row of round icon buttons on one pill-shaped surface, all the same
/// size: the header's actions, grouped the way a phone's title bar groups
/// them. Returns one response per button, in order.
pub fn action_capsule(ui: &mut Ui, buttons: &[CapsuleButton<'_>]) -> Vec<egui::Response> {
    let tok = theme::tokens(ui.visuals());
    let size = egui::vec2(action_capsule_width(buttons.len()), CAPSULE_H);
    let (rect, capsule) = ui.allocate_exact_size(size, Sense::hover());
    let visible = ui.is_rect_visible(rect);
    if visible {
        ui.painter()
            .rect_filled(rect, CAPSULE_H / 2.0, tok.surface_hi);
        ui.painter().rect_stroke(
            rect,
            CAPSULE_H / 2.0,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
    }
    buttons
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let r = egui::Rect::from_min_size(
                rect.min + egui::vec2(CAPSULE_PAD + i as f32 * CAPSULE_BUTTON, CAPSULE_PAD),
                egui::vec2(CAPSULE_BUTTON, CAPSULE_BUTTON),
            );
            let response = ui.interact(r, capsule.id.with(i), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    b.active,
                    b.label,
                )
            });
            if visible {
                let hovered = response.hovered();
                let (fill, color) = match (b.active, hovered) {
                    (true, _) => (tok.accent_tint, tok.accent),
                    (false, true) if b.danger => (tok.error_tint, tok.error),
                    (false, true) => (tok.hover_overlay, ui.visuals().text_color()),
                    (false, false) => (Color32::TRANSPARENT, tok.muted_text),
                };
                let painter = ui.painter();
                painter.circle_filled(r.center(), CAPSULE_BUTTON / 2.0, fill);
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    b.glyph,
                    FontId::new(16.0, egui::FontFamily::Proportional),
                    color,
                );
            }
            response.on_hover_text(b.label)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stateful controls
// ---------------------------------------------------------------------------

/// A pill-shaped on/off switch, 40x23. `name` is what a screen reader
/// announces for it: the label of the row it sits in.
pub fn named_toggle(ui: &mut Ui, on: &mut bool, name: &str) -> egui::Response {
    let size = egui::vec2(40.0, 23.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, name)
    });
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
fn segmented_width(ui: &Ui, labels: &[&str]) -> f32 {
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

/// A chip that switches one thing on and off, for a row of related
/// switches that would each be a row of their own otherwise: tinted with a
/// check when on, an outline when off.
pub fn chip_toggle(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let text = if *on {
        format!("{}  {label}", icon::CHECK)
    } else {
        label.to_string()
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text, seg_font(), ui.visuals().text_color());
    let size = egui::vec2(galley.size().x + SEG_PAD * 2.0, 28.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, stroke, color) = if *on {
            (tok.accent_tint, tok.accent.gamma_multiply(0.45), tok.accent)
        } else {
            (
                ui.visuals().extreme_bg_color,
                ui.visuals().widgets.inactive.bg_stroke.color,
                tok.muted_text,
            )
        };
        painter.rect_filled(rect, CornerRadius::same(R_CTRL), fill);
        painter.rect_stroke(
            rect,
            CornerRadius::same(R_CTRL),
            Stroke::new(1.0, stroke),
            StrokeKind::Inside,
        );
        if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(R_CTRL), tok.hover_overlay);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response
}

/// A slider drawn in the same hand as [`named_toggle`]: a thin recessed track, an
/// accent trail up to the value, a light knob. Fills the available width and
/// takes a click or a drag anywhere along the row; focused, the arrow keys
/// (and a screen reader's increment/decrement) move it by a hundredth of
/// the range. `name` is what a screen reader announces for it.
pub fn named_slider(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    name: &str,
) -> egui::Response {
    use egui::accesskit::Action;
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
    let focused = response.has_focus();
    if focused {
        // Left and right move the knob instead of the focus.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            )
        });
    }
    let steps = ui.input(|i| {
        let keys = if focused {
            i.num_presses(egui::Key::ArrowRight) as i32 - i.num_presses(egui::Key::ArrowLeft) as i32
        } else {
            0
        };
        keys + i.num_accesskit_action_requests(response.id, Action::Increment) as i32
            - i.num_accesskit_action_requests(response.id, Action::Decrement) as i32
    });
    let step = (range.end() - range.start()) / 100.0;
    if steps != 0 {
        let moved = (*value + steps as f32 * step).clamp(*range.start(), *range.end());
        if moved != *value {
            *value = moved;
            response.mark_changed();
        }
    }
    let shown = f64::from(*value);
    response.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), shown, name));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(f64::from(*range.start()));
        node.set_max_numeric_value(f64::from(*range.end()));
        node.set_numeric_value_step(f64::from(step));
        node.add_action(Action::Increment);
        node.add_action(Action::Decrement);
    });
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
        let hot = response.hovered() || response.dragged() || focused;
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

/// How wide [`speaking_bars`] are: five bars of 2, 2 apart.
pub const SPEAKING_BARS_W: f32 = 5.0 * 2.0 + 4.0 * 2.0;

/// Five static accent bars indicating "currently speaking". Always
/// allocates its footprint so rows don't reflow when this toggles.
pub fn speaking_bars(ui: &mut Ui) -> egui::Response {
    const HEIGHTS: [f32; 5] = [5.0, 11.0, 7.0, 13.0, 6.0];
    let w = 2.0;
    let gap = 2.0;
    let max_h = 13.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(SPEAKING_BARS_W, max_h), Sense::hover());
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
/// `speaking` draws an accent ring around it. `video` puts that person's live
/// picture into the circle instead of the initials — same slot, same
/// allocation, same ring, so a row does not move when a camera comes on.
pub fn avatar(
    ui: &mut Ui,
    name: &str,
    user_id: icu_proto::UserId,
    speaking: bool,
    size: f32,
    video: Option<egui::load::SizedTexture>,
) -> egui::Response {
    // The ring sits outside the circle; reserving room for it keeps the row
    // from shifting when somebody starts talking.
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size + 6.0, size + 6.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center = rect.center();
        match video {
            Some(tex) => {
                // 4:3 into a circle: keep the short side whole and crop the
                // long one symmetrically, so a face stays centered instead of
                // sliding towards one edge.
                let (w, h) = (tex.size.x.max(1.0), tex.size.y.max(1.0));
                let crop = if w >= h {
                    egui::vec2(h / w, 1.0)
                } else {
                    egui::vec2(1.0, w / h)
                };
                painter.add(
                    egui::epaint::RectShape::filled(
                        egui::Rect::from_center_size(center, egui::vec2(size, size)),
                        // The tessellator clamps a corner radius to half the
                        // shorter side, which turns this rect into the circle
                        // the initials would have drawn.
                        CornerRadius::same((size / 2.0).ceil() as u8),
                        Color32::WHITE,
                    )
                    .with_texture(
                        tex.id,
                        egui::Rect::from_center_size(egui::pos2(0.5, 0.5), crop),
                    ),
                );
            }
            None => {
                let palette = if ui.visuals().dark_mode {
                    &AVATAR_DARK
                } else {
                    &AVATAR_LIGHT
                };
                let (fill, text_color) = palette[(user_id % 6) as usize];
                painter.circle_filled(center, size / 2.0, fill);
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    initials(name),
                    FontId::new(size * 0.37, super::fonts::semibold()),
                    text_color,
                );
            }
        }
        if speaking {
            let tok = theme::tokens(ui.visuals());
            painter.circle_stroke(center, size / 2.0 + 2.0, Stroke::new(2.0, tok.accent));
        }
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
        paint_well(ui, rect, R_CHIP);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, tok.dim_text);
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

/// Horizontal padding inside a [`status_pill`].
const PILL_PAD: f32 = 9.0;
/// Diameter of a [`status_pill`]'s dot.
const PILL_DOT: f32 = 7.0;
/// Gap between the dot and the word.
const PILL_GAP: f32 = 5.0;

/// A rounded status pill: a colored dot plus one short word. Anything longer
/// than a word belongs in the caller's `on_hover_text`, not in here.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str) -> egui::Response {
    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, ui.visuals().text_color());
    let size = egui::vec2(
        PILL_PAD * 2.0 + PILL_DOT + PILL_GAP + galley.size().x,
        galley.size().y.max(PILL_DOT) + 7.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, color.gamma_multiply(0.16));
    painter.rect_stroke(
        rect,
        rect.height() / 2.0,
        Stroke::new(1.0, color.gamma_multiply(0.27)),
        StrokeKind::Inside,
    );
    painter.circle_filled(
        egui::pos2(rect.left() + PILL_PAD + PILL_DOT / 2.0, rect.center().y),
        PILL_DOT / 2.0,
        color,
    );
    let y = rect.center().y - galley.size().y / 2.0;
    painter.galley(
        egui::pos2(rect.left() + PILL_PAD + PILL_DOT + PILL_GAP, y),
        galley,
        ui.visuals().text_color(),
    );
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

/// One icon-rail entry.
pub fn rail_button(ui: &mut Ui, glyph: &str, label: &str, active: bool) -> bool {
    let tok = theme::tokens(ui.visuals());
    let text_color = if active { tok.accent } else { tok.muted_text };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(64.0, 38.0), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if active {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(theme::R_CTRL),
                ui.visuals().faint_bg_color,
            );
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(theme::R_CTRL),
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            egui::pos2(rect.center().x, rect.top() + 13.0),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::new(17.0, egui::FontFamily::Proportional),
            text_color,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 9.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::new(9.5, super::fonts::semibold()),
            text_color,
        );
    }
    resp.clicked()
}

/// Height of a [`nav_item`].
const NAV_H: f32 = 34.0;

/// One entry of the settings window's sidebar: icon and label side by side
/// at body size, full width of the sidebar. The active entry gets the accent
/// tint the header's toggled icon buttons use, so "where am I" reads the
/// same everywhere.
pub fn nav_item(ui: &mut Ui, glyph: &str, label: &str, active: bool) -> bool {
    let tok = theme::tokens(ui.visuals());
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), NAV_H), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, color) = if active {
            (tok.accent_tint, tok.accent)
        } else if resp.hovered() {
            (tok.hover_overlay, ui.visuals().text_color())
        } else {
            (Color32::TRANSPARENT, tok.muted_text)
        };
        painter.rect_filled(rect, CornerRadius::same(R_CTRL), fill);
        painter.text(
            egui::pos2(rect.left() + 20.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::new(17.0, egui::FontFamily::Proportional),
            color,
        );
        let font = if active {
            FontId::new(13.5, super::fonts::semibold())
        } else {
            FontId::new(13.5, egui::FontFamily::Proportional)
        };
        painter.text(
            egui::pos2(rect.left() + 38.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            if active {
                ui.visuals().text_color()
            } else {
                color
            },
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The top of a settings page: its name as a heading, one plain sentence
/// on what lives here, and optional actions right-aligned beside the title.
pub fn page_header<R>(
    ui: &mut Ui,
    title: &str,
    description: &str,
    actions: impl FnOnce(&mut Ui) -> R,
) -> R {
    let tok = theme::tokens(ui.visuals());
    let result = ui
        .horizontal(|ui| {
            ui.label(RichText::new(title).text_style(egui::TextStyle::Heading));
            rtl(ui, 30.0, actions)
        })
        .inner;
    ui.label(RichText::new(description).size(12.5).color(tok.muted_text));
    ui.add_space(12.0);
    result
}

/// Opens the expert part of a [`group_card`]: a divider and a small
/// "Erweitert" caption, so a row that only shows in expert mode is visibly
/// set apart from the everyday ones above it. The first row after it takes
/// `divider: false`.
pub fn advanced_caption(ui: &mut Ui) {
    let tok = theme::tokens(ui.visuals());
    ui.separator();
    Frame::new()
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 9,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("{}  ERWEITERT", icon::WRENCH))
                    .font(FontId::new(10.5, super::fonts::semibold()))
                    .color(tok.dim_text),
            );
        });
}

/// What a [`badge`] says about someone, which picks its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    /// Neutral fact ("du").
    Neutral,
    /// A role worth noticing ("Admin").
    Accent,
    /// Something is wrong or restricted ("Gesperrt").
    Danger,
    /// Waiting on somebody ("Eingeladen").
    Pending,
}

/// A small rounded label for a status or role, in proportional type — the
/// word-sized sibling of the monospace value [`chip`].
pub fn badge(ui: &mut Ui, text: &str, kind: BadgeKind) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (fill, border, color) = match kind {
        BadgeKind::Neutral => (
            ui.visuals().extreme_bg_color,
            ui.visuals().widgets.inactive.bg_stroke.color,
            tok.muted_text,
        ),
        BadgeKind::Accent => (tok.accent_tint, tok.accent.gamma_multiply(0.35), tok.accent),
        BadgeKind::Danger => (tok.error_tint, tok.error.gamma_multiply(0.35), tok.error),
        BadgeKind::Pending => (
            tok.warning_tint,
            tok.connecting.gamma_multiply(0.35),
            tok.connecting,
        ),
    };
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::new(11.0, super::fonts::semibold()),
        color,
    );
    let size = egui::vec2(galley.size().x + 14.0, galley.size().y + 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(R_CHIP), fill);
        painter.rect_stroke(
            rect,
            CornerRadius::same(R_CHIP),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response
}

/// A quiet square button with three dots, for a row's overflow menu.
/// Painted rather than taken from Phosphor: every "⋯" glyph it has sits on
/// a codepoint Inter also fills, and Inter comes first in the chain.
pub fn more_button(ui: &mut Ui) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Aktionen")
    });
    if ui.is_rect_visible(rect) {
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        let (fill, color) = if open {
            (tok.accent_tint, tok.accent)
        } else if response.hovered() {
            (tok.hover_overlay, ui.visuals().text_color())
        } else {
            (Color32::TRANSPARENT, tok.muted_text)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(R_CTRL), fill);
        for dx in [-5.5, 0.0, 5.5] {
            painter.circle_filled(rect.center() + egui::vec2(dx, 0.0), 1.8, color);
        }
    }
    response
}

/// A small disclosure triangle, pointing right when closed and down when
/// open — painted for the same reason as [`more_button`].
pub fn disclosure(ui: &mut Ui, open: bool, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    let c = rect.center();
    let points = if open {
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -4.0),
            c + egui::vec2(3.0, 0.0),
            c + egui::vec2(-2.0, 4.0),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// One entry of a popup menu: icon and label, quiet until hovered.
/// `danger` paints it in the error color for the destructive entry.
pub fn menu_item(ui: &mut Ui, glyph: &str, label: &str, danger: bool) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let color = if danger {
        tok.error
    } else {
        ui.visuals().text_color()
    };
    ui.add(
        egui::Button::new(RichText::new(caption(glyph, label)).size(13.0).color(color))
            .frame(false)
            .min_size(egui::vec2(0.0, 28.0)),
    )
}

/// A small presence dot at the lower right of an [`avatar`] drawn into
/// `avatar_rect`, ringed in the card color so it reads on any fill.
pub fn presence_dot(ui: &Ui, avatar_rect: egui::Rect, color: Color32) {
    let center = avatar_rect.right_bottom() - egui::vec2(7.0, 7.0);
    let painter = ui.painter();
    painter.circle_filled(center, 5.5, ui.visuals().faint_bg_color);
    painter.circle_filled(center, 4.0, color);
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
    let result = Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.inactive.bg_stroke.color,
        ))
        .corner_radius(CornerRadius::same(R_CARD))
        .show(ui, |ui| {
            // Rows carry their own margins; item spacing on top of them
            // would pad only the bottom of each row (between it and the next
            // divider), leaving every label above its row's center.
            ui.spacing_mut().item_spacing.y = 0.0;
            add_contents(ui)
        })
        .inner;
    ui.add_space(12.0);
    result
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
    Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            // The label goes straight into the row, where the placer centers
            // it on the control; the help text runs underneath at full width
            // rather than beside the label, where it would wrap under the
            // control.
            let result = ui
                .horizontal(|ui| {
                    ui.label(RichText::new(label).size(13.5));
                    rtl(ui, 30.0, control)
                })
                .inner;
            if let Some(h) = help {
                ui.add_space(1.0);
                ui.label(RichText::new(h).size(11.5).color(tok.dim_text));
            }
            result
        })
        .inner
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
                label_column(ui, LABEL_W, 22.0, label);
                rtl(ui, 22.0, |ui| {
                    chip(ui, &format(*value), reserve);
                    named_slider(ui, value, range, label)
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
