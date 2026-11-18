//! Live user → server → local user path, with public certificate evidence.
use super::{
    connection::{certificate_details, certificate_details_height, Tone, NONE},
    main_window::{self, moderation_menu, Participant},
    theme, widgets,
};
use crate::engine::{AppState, Command};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Certificate {
    Peer,
    Server,
    Own,
    Ca,
}

/// Live path and certificate selection for one room participant.
pub struct UserView {
    user_id: icu_proto::UserId,
    requested: Option<(icu_proto::Session, Instant)>,
    connection: Option<Instant>,
    moderation_generation: Option<u64>,
    moderation_requested: Option<Instant>,
    selected: Option<Certificate>,
    copied: Option<(String, Instant)>,
}

#[derive(Debug, Default)]
/// Actions requested by the per-user window.
pub struct UserOutput {
    /// Open the complete connection window.
    pub open_connection: bool,
    /// The selected participant is no longer online.
    pub left: bool,
    /// Content beyond the visible height, available by scrolling.
    pub overflow: f32,
}

impl UserView {
    /// Start a view for this user.
    pub fn new(user_id: icu_proto::UserId) -> Self {
        Self {
            user_id,
            requested: None,
            connection: None,
            moderation_generation: None,
            moderation_requested: None,
            selected: None,
            copied: None,
        }
    }
    /// Selected stable user identity.
    pub fn user_id(&self) -> icu_proto::UserId {
        self.user_id
    }

    /// Draw from the latest state and send user actions to the engine.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        video: &dyn Fn(icu_proto::Session) -> Option<egui::load::SizedTexture>,
        send: &impl Fn(Command),
    ) -> UserOutput {
        let mut out = UserOutput::default();
        let Some(p) = main_window::participant_for(state, self.user_id, video, None) else {
            out.left = true;
            return out;
        };
        let Some(me) = state
            .me
            .as_ref()
            .and_then(|m| main_window::participant_for(state, m.user_id, video, None))
        else {
            out.left = true;
            return out;
        };
        let connected_at = state.conn_info.as_ref().map(|info| info.connected_at);
        if self.connection != connected_at {
            self.requested = None;
            self.moderation_generation = None;
            self.moderation_requested = None;
            self.connection = connected_at;
        }
        let own = p.user_id == me.user_id;
        if !own && state.is_admin {
            self.moderation_generation
                .get_or_insert(state.user_list_generation);
            let loaded = self.moderation_generation != Some(state.user_list_generation)
                && p.moderation.is_some_and(|m| m.restriction.is_some());
            let every = Duration::from_secs(if loaded { 10 } else { 2 });
            if self
                .moderation_requested
                .is_none_or(|t| t.elapsed() >= every)
            {
                send(Command::RequestUserList);
                self.moderation_requested = Some(Instant::now());
            }
            ui.ctx().request_repaint_after(every);
        } else {
            self.moderation_generation = None;
            self.moderation_requested = None;
        }
        let peer_result = state
            .peers
            .iter()
            .find(|peer| peer.info.session == p.session)
            .and_then(|peer| peer.certificate.as_ref());
        if !own
            && peer_result.is_none()
            && self
                .requested
                .is_none_or(|(session, _)| session != p.session)
        {
            send(Command::RequestPeerCertificate(p.session));
            self.requested = Some((p.session, Instant::now()));
            ui.ctx().request_repaint_after(Duration::from_secs(8));
        }
        let timed_out = self
            .requested
            .is_some_and(|(_, t)| t.elapsed() >= Duration::from_secs(8));
        let info = state.conn_info.as_deref();
        let peer_cert = if own {
            info.map(|i| &i.client_cert)
        } else {
            peer_result
                .and_then(|r| r.as_ref().ok())
                .map(|c| c.as_ref())
        };
        let draw_page = |ui: &mut Ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 18,
                    right: 18,
                    top: 16,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    self.hero(ui, &p, own, send);
                    ui.add_space(14.0);
                    if let Some(v) = p.volume {
                        volume_section(ui, &p, v, send);
                    }
                    path_section(ui, state, &p, &me, own);
                    let link = self.certificate_section(
                        ui,
                        state,
                        &p,
                        own,
                        peer_cert,
                        peer_result,
                        timed_out,
                        send,
                    );
                    out.open_connection |= link;
                });
        };
        let page = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, draw_page);
        out.overflow = page.content_size.y - page.inner_rect.height();
        out
    }

    /// Avatar, name and what this person is doing — plain facts with a
    /// pictogram each instead of a row of chips. Moderation sits top right,
    /// apart from the facts, since it is the one thing here that acts.
    fn hero(&self, ui: &mut Ui, p: &Participant<'_>, own: bool, send: &impl Fn(Command)) {
        let tok = theme::tokens(ui.visuals());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            widgets::avatar(ui, p.name, p.user_id, p.speaking, p.ring, 64.0, p.video);
            ui.add_space(14.0);
            let moderation_w = if p.moderation.is_some() {
                widgets::button_width(ui, icon::GAVEL, "Moderation") + 12.0
            } else {
                0.0
            };
            let text_w = (ui.available_width() - moderation_w).max(120.0);
            ui.allocate_ui_with_layout(
                egui::vec2(text_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    ui.add_space(4.0);
                    let name = ui.add(
                        egui::Label::new(
                            RichText::new(p.name)
                                .font(egui::FontId::new(24.0, super::fonts::semibold())),
                        )
                        .wrap(),
                    );
                    name.on_hover_text(format!("Nutzer #{} · Sitzung {}", p.user_id, p.session));
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
                        let (color, status) = status_word(p, &tok);
                        status_dot(ui, color, status);
                        separator_dot(ui);
                        if p.is_admin {
                            fact(ui, icon::SHIELD_STAR, "Admin", tok.muted_text);
                        } else {
                            fact(ui, icon::USER, "Mitglied", tok.muted_text);
                        }
                        if own {
                            separator_dot(ui);
                            fact(ui, icon::USER_CIRCLE, "Das bist du", tok.muted_text);
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
                        if p.ptt {
                            fact(ui, icon::MICROPHONE, "Push-to-Talk", tok.muted_text);
                        } else {
                            fact(ui, icon::WAVEFORM, "Sprachaktivierung", tok.muted_text);
                        }
                        if p.camera {
                            ui.add_space(16.0);
                            fact(ui, icon::VIDEO_CAMERA, "Kamera an", tok.muted_text);
                        }
                        if p.viewing {
                            ui.add_space(16.0);
                            fact(ui, icon::EYE, "Schaut zu", tok.muted_text);
                        }
                    });
                },
            );
            if let Some(m) = p.moderation {
                widgets::rtl(ui, 30.0, |ui| {
                    let restriction = self
                        .moderation_generation
                        .filter(|generation| *generation != m.generation)
                        .and(m.restriction);
                    ui.menu_button(format!("{}  Moderation", icon::GAVEL), |ui| {
                        moderation_menu(ui, p.user_id, restriction, send)
                    });
                });
            }
        });
    }

    /// The four certificates as a list, one pictogram and one verdict each,
    /// with the picked one's details opening underneath. Returns whether
    /// „Alle Verbindungsdetails" was clicked.
    #[allow(clippy::too_many_arguments)]
    fn certificate_section(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        p: &Participant<'_>,
        own: bool,
        peer_cert: Option<&crate::conninfo::CertInfo>,
        peer_result: Option<&Result<std::sync::Arc<crate::conninfo::CertInfo>, String>>,
        timed_out: bool,
        send: &impl Fn(Command),
    ) -> bool {
        let info = state.conn_info.as_deref();
        let tok = theme::tokens(ui.visuals());
        let now = crate::net::conn::unix_now();
        let mut open_connection = false;
        section(
            ui,
            icon::SHIELD_CHECK,
            "Zertifikate & Vertrauen",
            None,
            0,
            |ui| {
                egui::Frame::new()
                .inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 9 })
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new(icon::LOCK_KEY).size(15.0).color(tok.connected));
                        ui.label("QUIC · TLS 1.3 · mTLS");
                        widgets::rtl(ui, 20.0, |ui| {
                            if ui.link("Alle Verbindungsdetails").clicked() {
                                open_connection = true;
                            }
                        });
                    });
                    ui.add_space(3.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Der Transport ist verschlüsselt. Der Server kann die weitergeleiteten Medien lesen.",
                            )
                            .size(11.5)
                            .color(tok.dim_text),
                        )
                        .wrap(),
                    );
                });
                let was_closed = self.selected.is_none();
                let mut list_rect = egui::Rect::NOTHING;
                for (pick, glyph, label) in [
                    (Certificate::Peer, icon::USER, "Nutzerzertifikat"),
                    (Certificate::Server, icon::HARD_DRIVES, "Serverzertifikat"),
                    (
                        Certificate::Own,
                        icon::IDENTIFICATION_CARD,
                        "Dein Zertifikat",
                    ),
                    (Certificate::Ca, icon::SEAL_CHECK, "CA"),
                ] {
                    if own && pick == Certificate::Peer {
                        continue;
                    }
                    let cert = match pick {
                        Certificate::Peer => peer_cert,
                        Certificate::Server => info.map(|i| &i.server_cert),
                        Certificate::Own => info.map(|i| &i.client_cert),
                        Certificate::Ca => info.map(|i| &i.ca_cert),
                    };
                    let summary = match cert {
                        Some(c) => CertSummary::of(c, pick == Certificate::Ca, now, &tok),
                        None if pick == Certificate::Peer
                            && (timed_out || peer_result.is_some_and(|r| r.is_err())) =>
                        {
                            CertSummary {
                                subtitle: "Nicht abrufbar".into(),
                                glyph: icon::WARNING_CIRCLE,
                                word: "Fehlt",
                                color: tok.error,
                            }
                        }
                        None => CertSummary {
                            subtitle: "Wird geladen …".into(),
                            glyph: icon::CIRCLE_NOTCH,
                            word: "Lädt",
                            color: tok.muted_text,
                        },
                    };
                    let selected = self.selected == Some(pick);
                    let row = cert_list_row(ui, glyph, label, &summary, selected);
                    list_rect = list_rect.union(row.rect);
                    if row.clicked() {
                        self.selected = (!selected).then_some(pick);
                        self.copied = None;
                    }
                }
                if let Some(selected) = self.selected {
                    let cert = match selected {
                        Certificate::Peer => peer_cert,
                        Certificate::Server => info.map(|i| &i.server_cert),
                        Certificate::Own => info.map(|i| &i.client_cert),
                        Certificate::Ca => info.map(|i| &i.ca_cert),
                    };
                    hairline(ui);
                    let height = certificate_details_height(ui);
                    let pane = egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(egui::CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: theme::R_CARD,
                            se: theme::R_CARD,
                        })
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.set_min_height(height);
                            if let Some(cert) = cert {
                                certificate_details(
                                    ui,
                                    cert,
                                    selected == Certificate::Ca,
                                    (selected == Certificate::Server)
                                        .then_some(state.server_name.as_str()),
                                    &mut self.copied,
                                );
                            } else {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(height / 2.0 - 34.0);
                                    let error = (selected == Certificate::Peer)
                                        .then(|| {
                                            peer_result
                                                .and_then(|r| r.as_ref().err())
                                                .map(String::as_str)
                                                .or(timed_out
                                                    .then_some("Zertifikatsabfrage ohne Antwort"))
                                        })
                                        .flatten();
                                    if let Some(message) = error {
                                        ui.label(
                                            RichText::new(icon::WARNING_CIRCLE)
                                                .size(22.0)
                                                .color(tok.error),
                                        );
                                        ui.add_space(4.0);
                                        ui.label(message);
                                        ui.add_space(8.0);
                                        if ui
                                            .button(format!(
                                                "{}  Erneut laden",
                                                icon::ARROW_CLOCKWISE
                                            ))
                                            .clicked()
                                        {
                                            send(Command::RequestPeerCertificate(p.session));
                                            self.requested = Some((p.session, Instant::now()));
                                            ui.ctx().request_repaint_after(Duration::from_secs(8));
                                        }
                                    } else {
                                        ui.label(
                                            RichText::new(icon::CIRCLE_NOTCH)
                                                .size(22.0)
                                                .color(tok.muted_text),
                                        );
                                        ui.add_space(4.0);
                                        ui.weak(if selected == Certificate::Peer {
                                            "Nutzerzertifikat wird geladen …"
                                        } else {
                                            "Zertifikat noch nicht verfügbar"
                                        });
                                    }
                                });
                            }
                        });
                    if was_closed {
                        ui.scroll_to_rect(list_rect.union(pane.response.rect), None);
                    }
                }
            },
        );
        open_connection
    }
}

/// The one-word status a `status_pill` shows: what this person is doing
/// right now, priority order speaking > deafened > muted > idle.
fn status_word(p: &Participant<'_>, tok: &theme::Tokens) -> (egui::Color32, &'static str) {
    if p.speaking {
        (tok.connected, "Spricht")
    } else if p.deafened {
        (tok.error, "Hört nicht zu")
    } else if p.muted {
        (tok.error, "Stummgeschaltet")
    } else {
        (tok.muted_text, "Verbunden")
    }
}

/// What this client loses of this person's audio, toned like the
/// Verbindung window tones transport loss: none of it is fine, a little is
/// a warning, a lot is a fault.
fn loss_word(loss: Option<u16>) -> (String, Tone) {
    match loss {
        None => (NONE.to_string(), Tone::Plain),
        Some(0) => ("0 ‰".to_string(), Tone::Good),
        Some(l) if l <= 20 => (format!("{l} ‰"), Tone::Warn),
        Some(l) => (format!("{l} ‰"), Tone::Bad),
    }
}

/// A section: pictogram and title above a card, instead of a caps caption.
/// `margin` is the card's inner margin; 0 for cards whose rows run edge to
/// edge.
fn section<R>(
    ui: &mut Ui,
    glyph: &str,
    title: &str,
    hint: Option<&str>,
    margin: i8,
    contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let tok = theme::tokens(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        ui.label(RichText::new(glyph).size(16.0).color(tok.accent));
        ui.label(theme::strong_text(title));
        if let Some(hint) = hint {
            ui.label(RichText::new(hint).size(12.0).color(tok.dim_text));
        }
    });
    ui.add_space(7.0);
    let inner = egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().widgets.inactive.bg_stroke.color,
        ))
        .corner_radius(egui::CornerRadius::same(theme::R_CARD))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            contents(ui)
        })
        .inner;
    ui.add_space(14.0);
    inner
}

/// A quiet fact: pictogram and word in one color, no background. One
/// allocation, so a wrapping line never parts the two; the word alone is
/// its accessible label.
fn fact(ui: &mut Ui, glyph: &str, text: &str, color: egui::Color32) {
    let glyph =
        ui.painter()
            .layout_no_wrap(glyph.to_string(), egui::FontId::proportional(15.0), color);
    let word = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::TextStyle::Body.resolve(ui.style()),
        color,
    );
    fact_shape(ui, glyph, 5.0, word, text);
}

/// The live status: a colored dot and the word, in that color — the same
/// single allocation as [`fact`].
fn status_dot(ui: &mut Ui, color: egui::Color32, text: &str) {
    let dot = ui
        .painter()
        .layout_no_wrap("●".to_string(), egui::FontId::proportional(9.0), color);
    let word = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::new(14.0, super::fonts::medium()),
        color,
    );
    fact_shape(ui, dot, 6.0, word, text);
}

fn fact_shape(
    ui: &mut Ui,
    glyph: std::sync::Arc<egui::Galley>,
    gap: f32,
    word: std::sync::Arc<egui::Galley>,
    label: &str,
) {
    let height = glyph.size().y.max(word.size().y);
    let size = egui::vec2(glyph.size().x + gap + word.size().x, height);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
    let painter = ui.painter();
    let gy = rect.center().y - glyph.size().y / 2.0;
    let wy = rect.center().y - word.size().y / 2.0;
    let gx = glyph.size().x;
    painter.galley(
        egui::pos2(rect.left(), gy),
        glyph,
        egui::Color32::PLACEHOLDER,
    );
    painter.galley(
        egui::pos2(rect.left() + gx + gap, wy),
        word,
        egui::Color32::PLACEHOLDER,
    );
}

/// The middle dot between two facts on one line.
fn separator_dot(ui: &mut Ui) {
    let tok = theme::tokens(ui.visuals());
    ui.add_space(9.0);
    ui.label(RichText::new("·").color(tok.dim_text));
    ui.add_space(9.0);
}

/// A full-width 1 px divider.
fn hairline(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
}

/// What a certificate row says about its certificate at a glance.
struct CertSummary {
    subtitle: String,
    glyph: &'static str,
    word: &'static str,
    color: egui::Color32,
}

impl CertSummary {
    fn of(c: &crate::conninfo::CertInfo, is_ca: bool, now: i64, tok: &theme::Tokens) -> Self {
        let subtitle = format!("{}  ·  bis {}", c.subject, c.not_after);
        let revoked = matches!(
            c.revocation,
            crate::conninfo::Revocation::Checked { revoked: true, .. }
        );
        let (glyph, word, color) = match c.validity_label(now) {
            _ if !is_ca && c.signature_valid == Some(false) => {
                (icon::X_CIRCLE, "Ungültig", tok.error)
            }
            _ if !is_ca && revoked => (icon::X_CIRCLE, "Widerrufen", tok.error),
            "Abgelaufen" => (icon::X_CIRCLE, "Abgelaufen", tok.error),
            "Noch nicht gültig" => (icon::X_CIRCLE, "Noch nicht gültig", tok.error),
            "Läuft bald ab" => (icon::WARNING_CIRCLE, "Läuft bald ab", tok.connecting),
            // Green only when the signature was checked too; a date alone
            // is no trust verdict.
            "Zeitlich gültig" if is_ca || c.signature_valid == Some(true) => {
                (icon::CHECK_CIRCLE, "Gültig", tok.connected)
            }
            "Zeitlich gültig" => (icon::CALENDAR_CHECK, "Zeitlich gültig", tok.muted_text),
            _ => (icon::QUESTION, "Unbekannt", tok.muted_text),
        };
        Self {
            subtitle,
            glyph,
            word,
            color,
        }
    }
}

/// Height of one row in the certificate list.
const CERT_LIST_ROW_H: f32 = 44.0;

/// One certificate in the list: pictogram, name over subject and expiry,
/// verdict and disclosure arrow on the right. Clicking opens its details.
fn cert_list_row(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    s: &CertSummary,
    selected: bool,
) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    hairline(ui);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), CERT_LIST_ROW_H),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label)
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, tok.accent_tint);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            tok.accent,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, 0.0, tok.hover_overlay);
    }
    let ink = if selected { tok.accent } else { tok.muted_text };
    painter.text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(20.0),
        ink,
    );
    // Right side first, so the subtitle knows where to stop.
    let caret = if selected {
        icon::CARET_DOWN
    } else {
        icon::CARET_RIGHT
    };
    let right = rect.right() - 14.0;
    painter.text(
        egui::pos2(right, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        caret,
        egui::FontId::proportional(14.0),
        tok.dim_text,
    );
    let word = painter.layout_no_wrap(
        s.word.to_string(),
        egui::FontId::proportional(12.5),
        s.color,
    );
    let word_left = right - 24.0 - word.size().x;
    painter.galley(
        egui::pos2(word_left, rect.center().y - word.size().y / 2.0),
        word,
        s.color,
    );
    painter.text(
        egui::pos2(word_left - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        s.glyph,
        egui::FontId::proportional(15.0),
        s.color,
    );
    let text_left = rect.left() + 50.0;
    let text_w = (word_left - 30.0 - text_left).max(40.0);
    let elided = |text: &str, font: egui::FontId, color| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
        ui.painter().layout_job(job)
    };
    let title = elided(
        label,
        egui::FontId::new(14.0, super::fonts::semibold()),
        ui.visuals().text_color(),
    );
    let subtitle = elided(
        &s.subtitle,
        egui::FontId::proportional(12.0),
        tok.muted_text,
    );
    let top = rect.center().y - (title.size().y + 2.0 + subtitle.size().y) / 2.0;
    let title_h = title.size().y;
    painter.galley(egui::pos2(text_left, top), title, ui.visuals().text_color());
    painter.galley(
        egui::pos2(text_left, top + title_h + 2.0),
        subtitle,
        tok.muted_text,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    widgets::focus_ring(ui, &response, 0);
    response.on_hover_text(format!("{label}: {}", s.word))
}

/// Height of the drawn connection path.
const PATH_H: f32 = 80.0;

/// One end or the middle of the drawn connection path.
enum Node<'a> {
    Person(&'a Participant<'a>, &'a str),
    Server(&'a str, &'a str),
}

/// Who reaches whom: this person, the server, you — drawn as three nodes
/// with the measured round trip on each link, and the audio loss under it.
fn path_section(
    ui: &mut Ui,
    state: &AppState,
    p: &Participant<'_>,
    me: &Participant<'_>,
    own: bool,
) {
    let tok = theme::tokens(ui.visuals());
    section(
        ui,
        icon::PULSE,
        "Verbindung",
        Some("über den Server"),
        0,
        |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 14,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    let server = Node::Server(&state.server_name, &state.server);
                    let (nodes, links): (Vec<Node<'_>>, Vec<Option<u16>>) = if own {
                        (vec![Node::Person(me, "Du"), server], vec![me.rtt_ms])
                    } else {
                        (
                            vec![Node::Person(p, p.name), server, Node::Person(me, "Du")],
                            vec![p.rtt_ms, me.rtt_ms],
                        )
                    };
                    path_diagram(ui, &nodes, &links);
                });
            hairline(ui);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if !own {
                        let (loss, tone) = loss_word(p.loss_permille);
                        let color = match tone {
                            Tone::Good => tok.connected,
                            Tone::Warn => tok.connecting,
                            Tone::Bad => tok.error,
                            _ => tok.muted_text,
                        };
                        info_row(
                            ui,
                            icon::WAVEFORM,
                            "Audio-Paketverlust bei dir",
                            &loss,
                            color,
                        );
                    } else {
                        let ping = me
                            .client_rtt_ms
                            .map_or_else(|| NONE.to_string(), |ms| format!("{ms} ms"));
                        info_row(
                            ui,
                            icon::TIMER,
                            "Eigene Ping-Messung",
                            &ping,
                            ui.visuals().text_color(),
                        );
                    }
                });
        },
    );
}

/// Pictogram and label left, value right.
fn info_row(ui: &mut Ui, glyph: &str, label: &str, value: &str, color: egui::Color32) {
    let tok = theme::tokens(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(glyph).size(15.0).color(tok.muted_text));
        ui.label(RichText::new(label).color(tok.muted_text));
        widgets::rtl(ui, 20.0, |ui| {
            ui.label(
                RichText::new(value)
                    .color(color)
                    .font(egui::FontId::new(14.0, super::fonts::medium())),
            );
        });
    });
}

fn path_diagram(ui: &mut Ui, nodes: &[Node<'_>], links: &[Option<u16>]) {
    let tok = theme::tokens(ui.visuals());
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, PATH_H), egui::Sense::hover());
    // The drawing has no text widgets; say the same thing to a screen reader.
    let mut summary = String::from("Verbindungspfad: ");
    for (i, node) in nodes.iter().enumerate() {
        summary.push_str(match node {
            Node::Person(_, title) => title,
            Node::Server(name, _) => name,
        });
        if let Some(rtt) = links.get(i) {
            let rtt = rtt.map_or_else(|| "RTT unbekannt".to_string(), |ms| format!("{ms} ms"));
            summary.push_str(&format!(" – {rtt} – "));
        }
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &summary));
    let slot = width / nodes.len() as f32;
    const R: f32 = 22.0;
    let cy = rect.top() + R + 4.0;
    let center = |i: usize| egui::pos2(rect.left() + slot * (i as f32 + 0.5), cy);
    let line = tok.dim_text.gamma_multiply(0.6);
    for (i, rtt) in links.iter().enumerate() {
        let (a, b) = (center(i), center(i + 1));
        let (x0, x1) = (a.x + R + 10.0, b.x - R - 10.0);
        let painter = ui.painter();
        painter.line_segment(
            [egui::pos2(x0, cy), egui::pos2(x1, cy)],
            egui::Stroke::new(1.5, line),
        );
        painter.circle_filled(egui::pos2(x0, cy), 2.5, line);
        painter.circle_filled(egui::pos2(x1, cy), 2.5, line);
        let text = rtt.map_or_else(|| NONE.to_string(), |ms| format!("{ms} ms"));
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.5), tok.muted_text);
        let bars_w = if rtt.is_some() {
            widgets::SIGNAL_BARS_W + 4.0
        } else {
            0.0
        };
        let total = bars_w + galley.size().x;
        let mid = (x0 + x1) / 2.0;
        let label_y = cy - 14.0;
        let left = mid - total / 2.0;
        if let Some(ms) = rtt {
            let bars = egui::Rect::from_min_size(
                egui::pos2(left, label_y - 6.0),
                egui::vec2(widgets::SIGNAL_BARS_W, 12.0),
            );
            widgets::signal_bars(ui, bars, main_window::rtt_bars(*ms));
        }
        let size = galley.size();
        ui.painter().galley(
            egui::pos2(left + bars_w, label_y - size.y / 2.0),
            galley,
            tok.muted_text,
        );
        let hit = egui::Rect::from_min_max(egui::pos2(x0, cy - 24.0), egui::pos2(x1, cy + 8.0));
        ui.interact(hit, ui.id().with(("path-link", i)), egui::Sense::hover())
            .on_hover_text("Round-Trip-Zeit, vom Server gemessen");
    }
    for (i, node) in nodes.iter().enumerate() {
        let c = center(i);
        let (title, hover) = match node {
            Node::Person(person, title) => {
                let r = egui::Rect::from_center_size(c, egui::vec2(2.0 * R + 8.0, 2.0 * R + 8.0));
                ui.put(r, |ui: &mut Ui| {
                    widgets::avatar(
                        ui,
                        person.name,
                        person.user_id,
                        false,
                        None,
                        2.0 * R,
                        person.video,
                    )
                });
                (
                    *title,
                    format!(
                        "{} · Nutzer #{} · Sitzung {}",
                        person.name, person.user_id, person.session
                    ),
                )
            }
            Node::Server(name, address) => {
                let painter = ui.painter();
                painter.circle_filled(c, R, tok.surface_hi);
                painter.circle_stroke(
                    c,
                    R,
                    egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                );
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    icon::HARD_DRIVES,
                    egui::FontId::proportional(20.0),
                    ui.visuals().text_color(),
                );
                (*name, (*address).to_string())
            }
        };
        let mut job = egui::text::LayoutJob::simple_singleline(
            title.to_string(),
            egui::FontId::new(13.5, super::fonts::medium()),
            ui.visuals().text_color(),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(slot - 12.0);
        let galley = ui.painter().layout_job(job);
        let pos = egui::pos2(c.x - galley.size().x / 2.0, c.y + R + 9.0);
        let label_rect = egui::Rect::from_min_size(pos, galley.size());
        ui.painter().galley(pos, galley, ui.visuals().text_color());
        let hit = egui::Rect::from_center_size(c, egui::vec2(2.0 * R, 2.0 * R)).union(label_rect);
        ui.interact(hit, ui.id().with(("path-node", i)), egui::Sense::hover())
            .on_hover_text(hover);
    }
}

/// The playback volume this client applies to this person: mute toggle,
/// slider and the value on one line.
fn volume_section(ui: &mut Ui, p: &Participant<'_>, mut v: f32, send: &impl Fn(Command)) {
    let tok = theme::tokens(ui.visuals());
    section(
        ui,
        icon::SPEAKER_HIGH,
        "Wiedergabelautstärke",
        Some("nur für dich"),
        0,
        |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(10, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        // One click to silence somebody for yourself, and back.
                        let muted = v <= 0.0;
                        let (glyph, label) = if muted {
                            (icon::SPEAKER_X, "Wieder hören")
                        } else {
                            (icon::SPEAKER_HIGH, "Für mich stummschalten")
                        };
                        let mute = widgets::icon_button(ui, glyph, label, 30.0, muted)
                            .on_hover_text(label);
                        if mute.clicked() {
                            send(Command::SetVolume(
                                p.user_id,
                                if muted { 100.0 } else { 0.0 },
                            ));
                        }
                        let value_w = 52.0;
                        let slider_w = (ui.available_width() - value_w - 10.0).max(60.0);
                        ui.allocate_ui(egui::vec2(slider_w, 30.0), |ui| {
                            let name = format!("Lautstärke {}", p.name);
                            if widgets::named_slider(ui, &mut v, 0.0..=200.0, &name).changed() {
                                send(Command::SetVolume(p.user_id, v));
                            }
                        });
                        ui.add_sized(
                            [value_w, 30.0],
                            egui::Label::new(
                                RichText::new(format!("{v:.0} %"))
                                    .font(egui::FontId::new(14.0, super::fonts::medium()))
                                    .color(if muted {
                                        tok.error
                                    } else {
                                        ui.visuals().text_color()
                                    }),
                            ),
                        );
                    });
                });
        },
    );
}
