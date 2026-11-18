//! Live user → server → local user path, with public certificate evidence.
use super::{
    connection::{certificate_details, Tone, NONE},
    main_window::{self, moderation_menu, Participant},
    theme, widgets,
};
use crate::engine::{AppState, Command};
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
        let certs = [
            peer_cert,
            info.map(|i| &i.server_cert),
            info.map(|i| &i.client_cert),
        ];
        let draw_page = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let tok = theme::tokens(ui.visuals());
            ui.horizontal_top(|ui| {
                widgets::avatar(ui, p.name, p.user_id, p.speaking, p.ring, 52.0, p.video);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(theme::strong_text(p.name).size(25.0)).wrap());
                    ui.horizontal_wrapped(|ui| {
                        let (color, status) = status_word(&p, &tok);
                        widgets::status_pill(ui, color, status);
                        ui.label(if p.is_admin { "Admin" } else { "Mitglied" });
                        ui.label(if p.ptt {
                            "Push-to-Talk"
                        } else {
                            "Sprachaktivierung"
                        });
                        if p.camera {
                            ui.label("Kamera aktiv");
                        }
                        if p.viewing {
                            ui.label("Schaut zu");
                        }
                        if let Some(m) = p.moderation {
                            let restriction = self
                                .moderation_generation
                                .filter(|generation| *generation != m.generation)
                                .and(m.restriction);
                            ui.menu_button("Moderation", |ui| {
                                moderation_menu(ui, p.user_id, restriction, send)
                            });
                        }
                    });
                });
            });
            if let Some(v) = p.volume {
                volume_card(ui, &p, v, send);
            }
            widgets::group_card(ui, "Verbindungspfad", |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    let width = ui.available_width();
                    ui.set_min_width(width);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let vertical = width < 32.0 * ui.text_style_height(&egui::TextStyle::Body);
                    let latency = |ms: Option<u16>| {
                        let arrow = if vertical { "↓" } else { "→" };
                        let reading = ms.map_or_else(|| "RTT unbekannt".into(), |v| format!("{v} ms RTT"));
                        format!("{arrow}  {reading}  {arrow}")
                    };
                    let route = |ui: &mut Ui| {
                        if !own {
                            ui.label(theme::strong_text(p.name)).on_hover_text(format!(
                                "Nutzer #{} · Sitzung {}",
                                p.user_id, p.session
                            ));
                            ui.weak(latency(p.rtt_ms));
                        } else {
                            ui.label("Du");
                            ui.weak(latency(me.rtt_ms));
                        }
                        ui.label(format!("{}  {}", icon::HARD_DRIVES, state.server_name))
                            .on_hover_text(&state.server);
                        if !own {
                            ui.weak(latency(me.rtt_ms));
                            ui.label("Du").on_hover_text(me.name);
                        }
                    };
                    if vertical {
                        ui.vertical(route);
                    } else {
                        ui.horizontal_wrapped(route);
                    }
                    if !own {
                        let (loss, tone) = loss_word(p.loss_permille);
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("Audio-Paketverlust bei dir");
                            let color = match tone {
                                Tone::Good => tok.connected,
                                Tone::Warn => tok.connecting,
                                Tone::Bad => tok.error,
                                _ => tok.muted_text,
                            };
                            ui.colored_label(color, loss);
                        });
                    }
                    if let Some(ms) = me.client_rtt_ms {
                        ui.response()
                            .on_hover_text(format!("Eigene Ping/Pong-Messung: {ms} ms"));
                    }
                });
            });
            widgets::group_card(ui, "Zertifikate & Vertrauen", |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (pick, label) in [
                            (Certificate::Peer, "Nutzerzertifikat"), (Certificate::Server, "Serverzertifikat"),
                            (Certificate::Own, "Dein Zertifikat"), (Certificate::Ca, "CA")
                        ] {
                            if own && pick == Certificate::Peer { continue; }
                            if ui.selectable_label(self.selected == Some(pick), format!("{} {label}", icon::CERTIFICATE)).clicked() {
                                self.toggle(pick);
                            }
                        }
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
                        let cert = match selected {
                            Certificate::Peer => certs[0], Certificate::Server => certs[1],
                            Certificate::Own => certs[2], Certificate::Ca => info.map(|i| &i.ca_cert),
                        };
                        ui.separator();
                        if let Some(cert) = cert {
                            certificate_details(ui, cert, selected == Certificate::Ca,
                                (selected == Certificate::Server).then_some(state.server_name.as_str()), &mut self.copied);
                        } else if selected != Certificate::Peer {
                            ui.weak("Zertifikat noch nicht verfügbar");
                        }
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{} QUIC · TLS 1.3 · mTLS", icon::LOCK_KEY)).small());
                        if ui.link("Alle Verbindungsdetails").clicked() { out.open_connection = true; }
                    });
                    ui.label(RichText::new("Der Transport ist verschlüsselt. Der Server kann die weitergeleiteten Medien lesen.").small());
                });
            });
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
                    let mute =
                        widgets::icon_button(ui, glyph, label, 24.0, muted).on_hover_text(label);
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
