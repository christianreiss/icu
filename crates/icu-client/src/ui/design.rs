//! Native controls matching the operator's ICU Desktop Redesign reference.
//! Keep these controls local to the main window and settings: other finished
//! windows retain their existing interaction and layout primitives.

use egui::{Color32, FontId, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use super::{fonts, theme, widgets};

pub const ACTION_SIZE: f32 = 44.0;
pub const ACTION_GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Settings,
    Power,
    Shield,
    Mic,
    MicOff,
    Speaker,
    Deaf,
    Camera,
    Overlay,
    Privacy,
    Keyboard,
    Users,
}

impl Icon {
    fn source(self) -> (&'static str, &'static [u8]) {
        macro_rules! asset {
            ($name:literal) => {
                (
                    concat!("bytes://icu-design/", $name, ".svg"),
                    include_bytes!(concat!("../../assets/design/", $name, ".svg")),
                )
            };
        }
        match self {
            Self::Settings => asset!("settings"),
            Self::Power => asset!("power"),
            Self::Shield => asset!("shield"),
            Self::Mic => asset!("mic"),
            Self::MicOff => asset!("mic-off"),
            Self::Speaker => asset!("speaker"),
            Self::Deaf => asset!("deaf"),
            Self::Camera => asset!("camera"),
            Self::Overlay => asset!("overlay"),
            Self::Privacy => asset!("privacy"),
            Self::Keyboard => asset!("keyboard"),
            Self::Users => asset!("users"),
        }
    }

    pub fn paint(self, ui: &Ui, rect: egui::Rect, color: Color32) {
        let (uri, bytes) = self.source();
        egui::Image::from_bytes(uri, bytes)
            .fit_to_exact_size(rect.size())
            .tint(color)
            .paint_at(ui, rect);
    }
}

pub fn surface(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(0x16, 0x17, 0x1a)
    } else {
        Color32::WHITE
    }
}

/// The reference deliberately distinguishes a live microphone's outline
/// from the quieter active camera/overlay tint, and solid Privacy from mute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionState {
    Off,
    Active,
    Muted,
    Sending,
    Privacy,
}

pub fn action(ui: &mut Ui, icon: Icon, label: &str, state: ActionState) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ACTION_SIZE), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            matches!(
                state,
                ActionState::Active | ActionState::Muted | ActionState::Privacy
            ),
            label,
        )
    });
    let t = theme::tokens(ui.visuals());
    let (fill, line, ink) = match state {
        ActionState::Off => (
            Color32::TRANSPARENT,
            ui.visuals().widgets.inactive.bg_stroke.color,
            t.muted_text,
        ),
        ActionState::Active => (t.accent_tint, t.accent_tint, t.accent),
        ActionState::Muted => (t.error_tint, t.error_tint, t.error),
        ActionState::Sending => (t.accent_tint, t.accent, t.accent),
        ActionState::Privacy => (
            t.error,
            t.error,
            if ui.visuals().dark_mode {
                Color32::from_rgb(0x1f, 0x0a, 0x0c)
            } else {
                Color32::WHITE
            },
        ),
    };
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect(rect, 10, fill, Stroke::new(1.0, line), StrokeKind::Inside);
        if response.hovered() {
            ui.painter().rect_filled(rect, 10, t.hover_overlay);
        }
        icon.paint(
            ui,
            egui::Rect::from_center_size(rect.center(), Vec2::splat(20.0)),
            ink,
        );
    }
    widgets::focus_ring(ui, &response, 10);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

pub fn header_action(ui: &mut Ui, icon: Icon, label: &str, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ACTION_SIZE), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let t = theme::tokens(ui.visuals());
    if ui.is_rect_visible(rect) {
        if active || response.hovered() {
            ui.painter().rect_filled(rect, 9, t.surface_hi);
        }
        icon.paint(
            ui,
            egui::Rect::from_center_size(rect.center(), Vec2::splat(20.0)),
            t.muted_text,
        );
    }
    widgets::focus_ring(ui, &response, 9);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

/// A one-pixel separator with four pixels of breathing room on either side.
pub fn action_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, ACTION_SIZE), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.center().y - 12.0..=rect.center().y + 12.0,
        Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
    );
}

pub fn mode_button(ui: &mut Ui, width: f32, title: &str, hint: &str, ink: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ACTION_SIZE), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Sendemodus wechseln: {title}. {hint}"),
        )
    });
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 8, theme::tokens(ui.visuals()).hover_overlay);
    }
    let draw = |text: &str, font: FontId, color, y| {
        let galley = egui::WidgetText::from(RichText::new(text).font(font).color(color))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                (width - 20.0).max(0.0),
                egui::TextStyle::Body,
            );
        ui.painter().galley(
            egui::pos2(rect.left() + 10.0, rect.top() + y),
            galley,
            color,
        );
    };
    draw(title, FontId::new(13.0, fonts::semibold()), ink, 7.0);
    draw(
        hint,
        FontId::proportional(11.5),
        theme::tokens(ui.visuals()).dim_text,
        24.0,
    );
    widgets::focus_ring(ui, &response, 8);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Plain 34px text actions from the Settings artboard. Existing action labels,
/// enabled states and confirmations remain at the call site.
pub fn button(ui: &mut Ui, kind: widgets::ButtonKind, glyph: &str, label: &str) -> Response {
    let t = theme::tokens(ui.visuals());
    let fg = ui.visuals().text_color();
    let (fill, line, ink) = match kind {
        widgets::ButtonKind::Primary => (fg, fg, ui.visuals().panel_fill),
        widgets::ButtonKind::Neutral => (
            Color32::TRANSPARENT,
            ui.visuals().widgets.inactive.bg_stroke.color,
            fg,
        ),
        widgets::ButtonKind::Danger => (t.error_tint, t.error_tint, t.error),
    };
    let label = if label.is_empty() { glyph } else { label };
    let response = ui.add(
        egui::Button::new(
            RichText::new(label)
                .font(FontId::new(13.0, fonts::medium()))
                .color(ink),
        )
        .fill(fill)
        .stroke(Stroke::new(1.0, line))
        .corner_radius(8)
        .min_size(egui::vec2(0.0, 34.0)),
    );
    if response.hovered() {
        ui.painter().rect_filled(response.rect, 8, t.hover_overlay);
    }
    widgets::focus_ring(ui, &response, 8);
    response
}

pub fn button_width(ui: &Ui, glyph: &str, label: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            if label.is_empty() { glyph } else { label }.into(),
            FontId::new(13.0, fonts::medium()),
            Color32::WHITE,
        )
        .size()
        .x
        + 24.0
}

pub fn settings_style(ui: &mut Ui) {
    let fill = surface(ui.visuals());
    let style = ui.style_mut();
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::new(13.0, fonts::medium()));
    style.spacing.interact_size.y = 34.0;
    style.visuals.disabled_alpha = 0.4;
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.visuals.widgets.inactive.weak_bg_fill = fill;
    style.visuals.text_edit_bg_color = Some(fill);
}

pub fn segmented_width(ui: &Ui, labels: &[&str]) -> f32 {
    let longest = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(
                    (*label).into(),
                    FontId::new(12.5, fonts::medium()),
                    Color32::WHITE,
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    ((longest + 20.0) * labels.len() as f32 + 3.0 * labels.len().saturating_sub(1) as f32 + 6.0)
        .max(260.0)
        .min(ui.available_width())
}

pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    current: &mut T,
    options: &[(T, &str)],
) -> Response {
    let labels: Vec<_> = options.iter().map(|(_, label)| *label).collect();
    let width = segmented_width(ui, &labels);
    let mut changed = false;
    let inner = widgets::row_group(ui, width, |ui| {
        egui::Frame::new()
            .fill(theme::tokens(ui.visuals()).surface_hi)
            .corner_radius(9)
            .inner_margin(3)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.spacing_mut().button_padding = egui::vec2(8.0, 0.0);
                ui.spacing_mut().interact_size.y = 28.0;
                let item_width = ((width - 6.0 - 3.0 * options.len().saturating_sub(1) as f32)
                    / options.len().max(1) as f32)
                    .max(1.0);
                for (value, label) in options {
                    let selected = *current == *value;
                    let ink = if selected {
                        ui.visuals().text_color()
                    } else {
                        theme::tokens(ui.visuals()).muted_text
                    };
                    let response = ui.add_sized(
                        [item_width, 28.0],
                        egui::Button::new(
                            RichText::new(*label)
                                .font(FontId::new(12.5, fonts::medium()))
                                .color(ink),
                        )
                        .fill(if selected {
                            ui.visuals().panel_fill
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(7),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::RadioButton,
                            ui.is_enabled(),
                            selected,
                            *label,
                        )
                    });
                    widgets::focus_ring(ui, &response, 7);
                    if response.clicked() && !selected {
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

pub fn nav_item(ui: &mut Ui, glyph: &str, label: &str, active: bool) -> bool {
    use egui_phosphor::regular as p;
    let icon = match glyph {
        p::MICROPHONE | p::WAVEFORM | p::SPEAKER_HIGH => Some(Icon::Mic),
        p::VIDEO_CAMERA => Some(Icon::Camera),
        p::SQUARES_FOUR => Some(Icon::Overlay),
        p::COMMAND => Some(Icon::Keyboard),
        p::GEAR_FINE => Some(Icon::Settings),
        p::USERS_THREE => Some(Icon::Users),
        _ => None,
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    let t = theme::tokens(ui.visuals());
    if active || response.hovered() {
        ui.painter().rect_filled(
            rect,
            8,
            if active {
                t.surface_hi
            } else {
                t.hover_overlay
            },
        );
    }
    let ink = if active {
        ui.visuals().text_color()
    } else {
        t.muted_text
    };
    let pos = egui::pos2(rect.left() + 19.0, rect.center().y);
    if let Some(icon) = icon {
        icon.paint(
            ui,
            egui::Rect::from_center_size(pos, Vec2::splat(18.0)),
            ink,
        );
    } else {
        ui.painter().text(
            pos,
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(18.0),
            ink,
        );
    }
    ui.painter().text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::new(13.5, fonts::medium()),
        ink,
    );
    widgets::focus_ring(ui, &response, 8);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

pub fn group_card<R>(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.push_id(title, |ui| {
        ui.label(
            RichText::new(title)
                .font(FontId::new(13.0, fonts::semibold()))
                .color(theme::tokens(ui.visuals()).muted_text),
        );
        ui.add_space(6.0);
        let result = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                contents(ui)
            })
            .inner;
        ui.add_space(28.0);
        result
    })
    .inner
}

pub fn setting_row<R>(
    ui: &mut Ui,
    divider: bool,
    label: &str,
    help: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.push_id(label, |ui| {
        if divider {
            ui.separator();
        }
        let inner = egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(0, 7))
            .show(ui, |ui| {
                let t = theme::tokens(ui.visuals());
                if ui.available_width() < 480.0 {
                    ui.label(RichText::new(label).size(14.0));
                    if let Some(help) = help {
                        ui.label(RichText::new(help).size(12.0).color(t.dim_text));
                    }
                    ui.add_space(6.0);
                    widgets::rtl(ui, 34.0, control)
                } else {
                    ui.horizontal(|ui| {
                        let label_width = (ui.available_width() - 276.0).max(120.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(label_width, 34.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_max_width(label_width);
                                ui.label(RichText::new(label).size(14.0));
                                if let Some(help) = help {
                                    ui.label(RichText::new(help).size(12.0).color(t.dim_text));
                                }
                            },
                        );
                        widgets::rtl(ui, 34.0, control)
                    })
                    .inner
                }
            });
        reveal_setting(ui, label, &inner.response);
        inner.inner
    })
    .inner
}

/// A compact boolean control, matching the reference's 18px checkbox.
pub fn named_toggle(ui: &mut Ui, on: &mut bool, name: &str) -> Response {
    let (hit, mut response) = ui.allocate_exact_size(egui::vec2(26.0, 34.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, name)
    });
    let rect = egui::Rect::from_center_size(hit.center(), Vec2::splat(18.0));
    let t = theme::tokens(ui.visuals());
    ui.painter().rect(
        rect,
        4,
        if *on { t.accent } else { surface(ui.visuals()) },
        Stroke::new(
            1.0,
            if *on {
                t.accent
            } else {
                ui.visuals().widgets.inactive.bg_stroke.color
            },
        ),
        StrokeKind::Inside,
    );
    if *on {
        let at = |x, y| rect.min + egui::vec2(x, y);
        ui.painter().add(egui::Shape::line(
            vec![at(4.0, 9.0), at(7.5, 12.5), at(14.0, 5.5)],
            Stroke::new(2.0, t.on_accent),
        ));
    }
    widgets::focus_ring(ui, &response, 4);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn slider_row(
    ui: &mut Ui,
    divider: bool,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) -> Response {
    setting_row(ui, divider, label, None, |ui| {
        let text = if [" ms", " %", " px"].contains(&suffix) {
            format!("{value:.0}{suffix}")
        } else {
            format!("{value:.1}{suffix}")
        };
        ui.label(RichText::new(text).monospace().size(12.0));
        widgets::named_slider(ui, value, range, label)
    })
}

pub fn jump_to(ctx: &egui::Context, label: &str) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("settings_jump"), label.to_owned()));
    ctx.request_repaint();
}

pub(super) fn reveal_setting(ui: &Ui, label: &str, response: &Response) {
    let target = ui.data(|d| d.get_temp::<String>(egui::Id::new("settings_jump")));
    if target.as_deref() == Some(label) {
        response.scroll_to_me(Some(egui::Align::Center));
        let now = ui.input(|i| i.time);
        ui.data_mut(|d| {
            d.remove::<String>(egui::Id::new("settings_jump"));
            d.insert_temp(egui::Id::new("settings_highlight"), (response.id, now));
        });
        ui.ctx().request_repaint();
    }
    if ui
        .data(|d| d.get_temp::<(egui::Id, f64)>(egui::Id::new("settings_highlight")))
        .is_some_and(|(id, since)| id == response.id && ui.input(|i| i.time) - since < 3.0)
    {
        ui.painter().rect_stroke(
            response.rect.expand(3.0),
            8,
            Stroke::new(1.5, theme::tokens(ui.visuals()).accent),
            StrokeKind::Outside,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(3));
    }
}

pub fn autotune_control(
    ui: &mut Ui,
    audio: &mut crate::config::AudioSettings,
    state: &crate::engine::AppState,
    cmds: &mut Vec<crate::engine::Command>,
) -> egui::Response {
    let status = &state.tuning_status;
    let health = state.effective_audio_health(audio);
    let toggle = ui
        .add_enabled_ui(audio.denoise, |ui| {
            named_toggle(ui, &mut audio.auto_level, "Pegel automatisch")
        })
        .inner;
    use crate::audio::health::State;
    let (symbol, color) = match health.state {
        State::Good => ("✓", egui::Color32::from_rgb(50, 160, 95)),
        State::InputClipping | State::Limited | State::SoftwareOverload => {
            ("!", egui::Color32::from_rgb(220, 75, 65))
        }
        State::Held | State::Disabled => ("Ⅱ", ui.visuals().weak_text_color()),
        _ => ("…", egui::Color32::from_rgb(200, 150, 45)),
    };
    ui.label(egui::RichText::new(symbol).color(color))
        .on_hover_text(health.text())
        .widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), health.text())
        });
    let details = button(ui, widgets::ButtonKind::Neutral, "", "Details");
    egui::Popup::menu(&details).show(|ui| {
        ui.set_max_width(360.0);
        ui.label(health.text());
        if let Some(age)=health.feedback_age_secs { ui.small(format!("Empfangsmessung vor {age:.1} s")); }
        if let Some(at)=health.profile_saved_at {
            if let Ok(date)=time::OffsetDateTime::from_unix_timestamp(at as i64) { ui.small(format!("Guter Zustand gespeichert: {}",date.date())); }
        }
        ui.checkbox(&mut audio.remember_good_levels,"Gute Mikrofonpegel merken");
        if ui.add_enabled_ui(state.can_restore_audio, |ui| button(ui, widgets::ButtonKind::Neutral, "", "Letzten guten Zustand wiederherstellen")).inner.clicked() {
            cmds.push(crate::engine::Command::RestoreGoodAudio); ui.close();
        }
        if button(ui, widgets::ButtonKind::Neutral, "", "Für dieses Mikrofon neu lernen").clicked() {
            cmds.push(crate::engine::Command::ForgetGoodAudio); ui.close();
        }
        if button(ui, widgets::ButtonKind::Neutral, "", "Audio-Diagnose kopieren").clicked() { ui.ctx().copy_text(health.diagnostic()); }
        ui.separator();
        ui.add_enabled_ui(audio.auto_level && audio.denoise, |ui| {
            ui.checkbox(&mut audio.peer_assistance,"Pegel mit Empfangsfeedback")
                .on_hover_text("Empfänger bestätigen deinen Sprachpegel vor ihren eigenen Lautstärkereglern.");
            ui.checkbox(&mut audio.tune_system_mic,"Systemmikrofon automatisch");
        });
        ui.label("Systempegel gelten auch für andere Apps. ICU stellt seine Änderungen nach dem Gespräch zurück und respektiert manuelle Änderungen.");
        if !status.is_empty() {ui.separator();ui.small(status);}
    });
    details.on_hover_text(if status.is_empty() {
        "Optionen zur automatischen Pegelregelung"
    } else {
        status
    });
    toggle
}
