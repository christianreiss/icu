//! Live user → server → local user path, with public certificate evidence.
use super::{
    connection::{certificate_details, Tone, NONE},
    main_window::{self, moderation_menu, Participant},
    theme, widgets,
};
use crate::{
    conninfo::{CertInfo, Revocation},
    engine::{AppState, Command},
};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// Vertical overflow used by the common window sizing policy.
    pub overflow: f32,
}

impl UserView {
    /// Start a view for this user.
    pub fn new(user_id: icu_proto::UserId) -> Self {
        Self {
            user_id,
            requested: None,
            connection: None,
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
            self.connection = connected_at;
        }
        let own = p.user_id == me.user_id;
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
        let certs = [
            peer_cert,
            info.map(|i| &i.server_cert),
            info.map(|i| &i.client_cert),
        ];
        let draw_page = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GUTTER, 4.0);
            ui.label(theme::strong_text("Verbindungspfad").size(18.0));
            ui.add_space(8.0);
            let cards = path_cards(ui, &p, &me, state, certs, own);
            ui.add_space(8.0);
            path_links(ui, cards, p.rtt_ms, me.rtt_ms, own);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                    if own {
                        ui.label(
                            RichText::new(
                                "Beide Außenkarten zeigen dich und dieselbe Verbindung zum Server.",
                            )
                            .small(),
                        );
                    } else {
                        let (loss, tone) = loss_word(p.loss_permille);
                        let tok = theme::tokens(ui.visuals());
                        let color = match tone {
                            Tone::Good => tok.connected,
                            Tone::Warn => tok.connecting,
                            Tone::Bad => tok.error,
                            _ => tok.muted_text,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Empfang").color(tok.muted_text));
                            ui.add(egui::Label::new(format!("{} bei dir", p.name)).wrap());
                            ui.separator();
                            ui.label(RichText::new("Audio-Paketverlust").color(tok.muted_text));
                            ui.label(RichText::new(loss).color(color));
                        });
                    }
                });
            ui.add_space(8.0);
            widgets::group_card(ui, "Zertifikate & Vertrauen", |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(GUTTER, 4.0);
                    ui.vertical_centered(|ui| {
                        let ca = info.map(|i| &i.ca_cert);
                        let name = ca.map_or("unbekannt", |c| c.subject.as_str());
                        if ui.selectable_label(self.selected == Some(Certificate::Ca), format!("{} CA · {}", icon::SHIELD_CHECK, name)).clicked() {
                            self.toggle(Certificate::Ca);
                        }
                        ui.label(RichText::new("Vertrauensanker aus deinem Passport").small());
                    });
                    issuer_links(ui, cards.map(|r| r.center().x));
                    aligned_row(ui, "certificate-selectors", cards, |ui, i| {
                        let (pick, label) = [(Certificate::Peer, if own { "Dein Zertifikat" } else { "Nutzerzertifikat" }), (Certificate::Server, "Serverzertifikat"), (Certificate::Own, "Dein Zertifikat")][i];
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            if ui.selectable_label(self.selected == Some(pick), format!("{} {}", icon::CERTIFICATE, label)).clicked() { self.toggle(pick); }
                        });
                    });
                    if !own {
                        let error = peer_result.and_then(|r| r.as_ref().err()).map(String::as_str);
                        if let Some(message) = error.or((peer_result.is_none() && timed_out).then_some("Zertifikatsabfrage ohne Antwort")) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(message);
                                if ui.button("Erneut laden").clicked() {
                                    send(Command::RequestPeerCertificate(p.session));
                                    self.requested = Some((p.session, Instant::now()));
                                    ui.ctx().request_repaint_after(Duration::from_secs(8));
                                }
                            });
                        } else if peer_result.is_none() {
                            ui.label("Nutzerzertifikat wird geladen …");
                        }
                    }
                    if let Some(selected) = self.selected {
                        ui.add_space(6.0);
                        let (title, cert) = match selected {
                            Certificate::Peer => ("Nutzerzertifikat", certs[0]),
                            Certificate::Server => ("Serverzertifikat", certs[1]),
                            Certificate::Own => ("Dein Zertifikat", certs[2]),
                            Certificate::Ca => ("CA · Vertrauensanker", info.map(|i| &i.ca_cert)),
                        };
                        widgets::group_card(ui, title, |ui| {
                            egui::Frame::new().inner_margin(12).show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 8.0;
                                if selected != Certificate::Ca {
                                    ui.label(if selected == Certificate::Server {
                                        "Serveridentität im TLS-Handshake geprüft"
                                    } else {
                                        "Nutzersitzung vom Server authentifiziert"
                                    });
                                }
                                if selected == Certificate::Server {
                                    ui.label(format!("Geprüfter Servername: {}", state.server_name));
                                }
                                if let Some(cert) = cert {
                                    trust_details(ui, cert, selected == Certificate::Ca);
                                }
                            });
                            certificate_details(ui, cert, &mut self.copied);
                        });
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{} QUIC · TLS 1.3 · mTLS", icon::LOCK_KEY)).small());
                        if ui.link("Alle Verbindungsdetails").clicked() { out.open_connection = true; }
                    });
                    ui.label(RichText::new("Beide Strecken sind transportverschlüsselt. Der Server kann die weitergeleiteten Medien lesen.").small());
                });
            });
            if let Some(v) = p.volume {
                volume_card(ui, &p, v, send);
            }
            if let Some(m) = p.moderation {
                widgets::group_card(ui, "Moderation", |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            moderation_menu(ui, p.user_id, m, send);
                        });
                });
            }
        };
        let page = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, draw_page);
        out.overflow = page.content_size.y - page.inner_rect.height();
        super::grow_to_fit(ui.ctx(), out.overflow);
        out
    }

    fn toggle(&mut self, cert: Certificate) {
        self.selected = if self.selected == Some(cert) {
            None
        } else {
            Some(cert)
        };
    }
}

const GUTTER: f32 = 12.0;
const PADDING: f32 = 12.0;

/// Render a row across the same three column bounds. The next row starts
/// below the tallest cell, including any wrapped text, in this very frame.
fn aligned_row(
    ui: &mut Ui,
    id: &str,
    columns: [egui::Rect; 3],
    mut content: impl FnMut(&mut Ui, usize),
) {
    let top = ui.cursor().top();
    let mut bottom = top;
    for (i, column) in columns.into_iter().enumerate() {
        let rect = egui::Rect::from_min_max(
            egui::pos2(column.left() + PADDING, top),
            egui::pos2(column.right() - PADDING, ui.max_rect().bottom()),
        );
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((id, i))
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        cell.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        content(&mut cell, i);
        bottom = bottom.max(cell.min_rect().bottom());
    }
    ui.allocate_space(egui::vec2(ui.available_width(), bottom - top));
}

fn path_cards(
    ui: &mut Ui,
    peer: &Participant<'_>,
    me: &Participant<'_>,
    state: &AppState,
    certs: [Option<&CertInfo>; 3],
    own: bool,
) -> [egui::Rect; 3] {
    let width = (ui.available_width() - 2.0 * GUTTER) / 3.0;
    let origin = ui.cursor().min;
    let mut cards = std::array::from_fn(|i| {
        egui::Rect::from_min_size(
            origin + egui::vec2(i as f32 * (width + GUTTER), 0.0),
            egui::vec2(width, 0.0),
        )
    });
    // Reserve paint order before drawing the cells, then fill the complete
    // equal-height card backgrounds once all shared rows have been measured.
    let backgrounds: [_; 3] = std::array::from_fn(|_| ui.painter().add(egui::Shape::Noop));
    let tok = theme::tokens(ui.visuals());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.add_space(PADDING);
        for row in 0..7 {
            aligned_row(ui, &format!("path-row-{row}"), cards, |ui, i| {
                let person = [Some(peer), None, Some(me)][i];
                match row {
                    0 => {
                        ui.label(theme::caps_text(
                            [
                                if own {
                                    "Du · ausgewählt"
                                } else {
                                    "Ausgewählter Nutzer"
                                },
                                "Server · Vermittler",
                                "Du · lokal",
                            ][i],
                        ));
                    }
                    1 => {
                        ui.horizontal_top(|ui| {
                            if let Some(p) = person {
                                widgets::avatar(
                                    ui, p.name, p.user_id, p.speaking, p.ring, 38.0, p.video,
                                );
                            } else {
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(44.0, 44.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    r.center(),
                                    19.0,
                                    tok.muted_text.gamma_multiply(0.12),
                                );
                                ui.painter().text(
                                    r.center(),
                                    egui::Align2::CENTER_CENTER,
                                    icon::HARD_DRIVES,
                                    egui::FontId::proportional(24.0),
                                    tok.muted_text,
                                );
                            }
                            ui.vertical(|ui| {
                                // Align first name baselines even if another
                                // name wraps to several lines next to its icon.
                                ui.add_space(12.0);
                                ui.add(
                                    egui::Label::new(
                                        theme::strong_text(
                                            person.map_or(state.server_name.as_str(), |p| p.name),
                                        )
                                        .size(15.0),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    }
                    2 => {
                        ui.label(
                            RichText::new(person.map_or("Medienvermittlung", |p| {
                                if p.is_admin {
                                    "Admin"
                                } else {
                                    "Mitglied"
                                }
                            }))
                            .small()
                            .color(tok.muted_text),
                        );
                    }
                    3 => {
                        let (color, word) =
                            person.map_or((tok.connected, "Verbunden"), |p| status_word(p, &tok));
                        widgets::status_pill(ui, color, word);
                    }
                    4 => {
                        if let Some(p) = person {
                            ui.spacing_mut().interact_size.y =
                                ui.text_style_height(&egui::TextStyle::Body);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!(
                                    "{} {}",
                                    if p.muted {
                                        icon::MICROPHONE_SLASH
                                    } else {
                                        icon::MICROPHONE
                                    },
                                    if p.ptt { "PTT" } else { "Sprachaktivierung" }
                                ));
                                if p.camera {
                                    ui.label(format!("{} Kamera", icon::VIDEO_CAMERA));
                                }
                                if p.viewing {
                                    ui.label(format!("{} Schaut zu", icon::EYE));
                                }
                            });
                        } else {
                            ui.add(egui::Label::new(&state.server).wrap());
                        }
                    }
                    5 => {
                        if let Some(p) = person {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "Nutzer #{} · Sitzung {}",
                                        p.user_id, p.session
                                    ))
                                    .small()
                                    .color(tok.muted_text),
                                )
                                .wrap(),
                            )
                            .on_hover_text(
                                p.client_rtt_ms.map_or_else(
                                    || "Vom Server gemeldete Sitzung".into(),
                                    |ms| format!("Eigene Ping/Pong-Messung: {ms} ms"),
                                ),
                            );
                        }
                    }
                    6 => {
                        certificate_status(ui, certs[i]);
                    }
                    _ => unreachable!(),
                }
            });
        }
        ui.add_space(PADDING - 8.0);
    });
    let bottom = ui.min_rect().bottom();
    for (i, card) in cards.iter_mut().enumerate() {
        card.max.y = bottom;
        ui.painter().set(
            backgrounds[i],
            egui::epaint::RectShape::new(
                *card,
                egui::CornerRadius::same(theme::R_CARD),
                ui.visuals().faint_bg_color,
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                egui::StrokeKind::Inside,
            ),
        );
    }
    cards
}

fn path_links(
    ui: &mut Ui,
    cards: [egui::Rect; 3],
    remote: Option<u16>,
    local: Option<u16>,
    own: bool,
) {
    let tok = theme::tokens(ui.visuals());
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
    let centers = cards.map(|r| r.center().x);
    for i in 0..2 {
        let start = egui::pos2(centers[i], rect.center().y);
        let end = egui::pos2(centers[i + 1], rect.center().y);
        let stroke = egui::Stroke::new(1.5, tok.dim_text);
        ui.painter().line_segment([start, end], stroke);
        for (p, direction) in [(start, 1.0), (end, -1.0)] {
            for dy in [-4.0, 4.0] {
                ui.painter()
                    .line_segment([p, p + egui::vec2(6.0 * direction, dy)], stroke);
            }
        }
    }
    let top = ui.cursor().top();
    let mut bottom = top;
    for (i, ms) in [remote, local].into_iter().enumerate() {
        let bounds = egui::Rect::from_min_max(
            egui::pos2(centers[i], top),
            egui::pos2(centers[i + 1], ui.max_rect().bottom()),
        );
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("rtt", i))
                .max_rect(bounds)
                .layout(egui::Layout::top_down(egui::Align::Center)),
        );
        let number = ms.map_or("unbekannt".into(), |v| format!("{v} ms"));
        let text = format!("{} RTT zum Server · {number}", icon::LOCK_KEY);
        let bars_width = widgets::SIGNAL_BARS_W + 8.0;
        let galley = cell.painter().layout(
            text,
            egui::TextStyle::Body.resolve(cell.style()),
            cell.visuals().text_color(),
            (bounds.width() - bars_width).max(1.0),
        );
        let (r, response) = cell.allocate_exact_size(
            egui::vec2(bars_width + galley.size().x, galley.size().y.max(20.0)),
            egui::Sense::hover(),
        );
        if let Some(ms) = ms {
            widgets::signal_bars(
                &cell,
                egui::Rect::from_min_size(
                    r.left_center() - egui::vec2(0.0, 10.0),
                    egui::vec2(widgets::SIGNAL_BARS_W, 20.0),
                ),
                main_window::rtt_bars(ms),
            );
        }
        cell.painter().galley(
            egui::pos2(r.left() + bars_width, r.center().y - galley.size().y / 2.0),
            galley,
            cell.visuals().text_color(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                format!("RTT zum Server · {number}"),
            )
        });
        response.on_hover_text(if own {
            "Dieselbe Verbindung"
        } else {
            ping_word(ms)
        });
        bottom = bottom.max(cell.min_rect().bottom());
    }
    ui.allocate_space(egui::vec2(ui.available_width(), bottom - top));
}

fn issuer_links(ui: &mut Ui, xs: [f32; 3]) {
    let (r, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let stroke = egui::Stroke::new(1.0, theme::tokens(ui.visuals()).dim_text);
    ui.painter()
        .line_segment([r.center_top(), r.center()], stroke);
    ui.painter().line_segment(
        [
            egui::pos2(xs[0], r.center().y),
            egui::pos2(xs[2], r.center().y),
        ],
        stroke,
    );
    for x in xs {
        ui.painter().line_segment(
            [egui::pos2(x, r.center().y), egui::pos2(x, r.bottom())],
            stroke,
        );
    }
}

fn certificate_status(ui: &mut Ui, cert: Option<&CertInfo>) {
    let tok = theme::tokens(ui.visuals());
    let now = crate::net::conn::unix_now();
    let (text, color) = match cert {
        None => ("Zertifikat ausstehend", tok.muted_text),
        Some(c) if c.signature_valid == Some(false) => ("CA-Signatur ungültig", tok.error),
        Some(c) if matches!(c.revocation, Revocation::Checked { revoked: true, .. }) => {
            ("Zertifikat widerrufen", tok.error)
        }
        Some(c)
            if c.valid_from.is_some_and(|t| t > now) || c.valid_until.is_some_and(|t| t <= now) =>
        {
            (c.validity_label(now), tok.error)
        }
        Some(c) if c.validity_label(now) == "Läuft bald ab" => {
            ("Zertifikat läuft bald ab", tok.connecting)
        }
        Some(c) if c.signature_valid == Some(true) => ("CA-Signatur geprüft", tok.connected),
        Some(_) => ("CA-Signatur ungeprüft", tok.muted_text),
    };
    ui.add(
        egui::Label::new(
            RichText::new(format!("{} {}", icon::CERTIFICATE, text))
                .small()
                .color(color),
        )
        .wrap(),
    );
}

fn trust_details(ui: &mut Ui, cert: &CertInfo, is_ca: bool) {
    let now = crate::net::conn::unix_now();
    ui.label(cert.validity_label(now));
    if is_ca {
        ui.label("Lokal hinterlegte CA aus deinem Passport");
    } else {
        ui.label(match cert.signature_valid {
            Some(true) => "Signatur gegen deine CA geprüft",
            Some(false) => "Signatur passt nicht zu deiner CA",
            None => "Signatur nicht prüfbar",
        });
        ui.label(cert.revocation.label(now));
        ui.label(
            "Die CA-Prüfung bestätigt das Zertifikat. Die Zuordnung zur Sitzung meldet der Server.",
        );
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

/// A word for `rtt_bars`' 1–5, the same scale the list's signal bars use.
fn ping_word(ms: Option<u16>) -> &'static str {
    match ms {
        None => "Unbekannt",
        Some(ms) => match main_window::rtt_bars(ms) {
            5 => "Ausgezeichnet",
            4 => "Gut",
            3 => "Mäßig",
            _ => "Schlecht",
        },
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

/// The playback volume this client applies to this person — moved
/// verbatim from the old inline accordion, only in a card of its own.
fn volume_card(ui: &mut Ui, p: &Participant<'_>, mut v: f32, send: &impl Fn(Command)) {
    widgets::group_card(ui, "Lautstärke (nur für dich)", |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                ui.horizontal(|ui| {
                    ui.label("Wiedergabelautstärke");
                    widgets::chip(ui, &format!("{v:.0} %"), 46.0);
                    // One click to silence somebody for yourself, and back.
                    let muted = v <= 0.0;
                    let glyph = if muted {
                        icon::SPEAKER_X
                    } else {
                        icon::SPEAKER_HIGH
                    };
                    let label = if muted {
                        "Wieder hören"
                    } else {
                        "Für mich stummschalten"
                    };
                    let mute = widgets::icon_button(ui, glyph, 24.0, muted).on_hover_text(label);
                    mute.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
                    });
                    if mute.clicked() {
                        send(Command::SetVolume(
                            p.user_id,
                            if muted { 100.0 } else { 0.0 },
                        ));
                    }
                });
                ui.add_space(6.0);
                let name = format!("Lautstärke {}", p.name);
                if widgets::named_slider(ui, &mut v, 0.0..=200.0, &name).changed() {
                    send(Command::SetVolume(p.user_id, v));
                }
            });
    });
}
